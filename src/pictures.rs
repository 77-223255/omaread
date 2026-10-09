//! The pictures of a chapter: the room they take and the ones drawn.
//!
//! The layout needs every picture's size before a single byte of pixels is
//! read, and the view needs the pictures it can see decoded and no others.
//! Both live here so a chapter of hundreds of pictures costs one header read
//! per picture up front and a few milliseconds per frame after that.
//!
//! Decoding itself runs off the drawing thread: the bytes are read here,
//! handed over whole, and the picture takes its place when the answer comes
//! back — a frame or two later, into rows the layout had already reserved for
//! it, so the reader sees the text at once and the picture where it belongs.

use crate::doc::Chapter;
use crate::epub::Book;
use crate::layout::{self, Line};

/// One picture of the chapter: where it sits, where its bytes are, how much
/// room it takes, and why it is sized that way. Known before anything is
/// decoded.
pub(crate) struct ImageSlot {
    pub(crate) block: usize,
    pub(crate) src: String,
    pub(crate) rows: u16,
    /// Cells across, as `image::measure` counted them for this room. The view
    /// needs no more than the height, but the layout indents the picture to
    /// the centre of the column from this.
    pub(crate) cols: u16,
    pub(crate) reason: crate::image::Reason,
}

/// The picture pipeline of one chapter: measured slots, decoded pictures, and
/// what this terminal draws them with.
pub(crate) struct Pictures {
    /// Pictures on screen, by block. Only what the view shows is kept: a
    /// chapter of rendered formulas holds hundreds of pictures, and decoding
    /// them all took twenty seconds before the first line appeared.
    pub(crate) rendered: std::collections::HashMap<usize, crate::image::Rendered>,
    /// Every picture of the chapter with the size it will take, measured from
    /// the image headers — the first 64 KB of each entry, nothing more. The
    /// layout needs all of them; decoding does not.
    pub(crate) slots: Vec<ImageSlot>,
    /// Pictures of this chapter whose bytes turned out to be undecodable: a
    /// header that measures is not a picture that draws. Their blocks must
    /// reserve nothing, and this chapter does not ask about them again —
    /// once a header has lied, re-reading it on every resize would only
    /// reserve the blank rows again.
    pub(crate) unreadable: std::collections::HashSet<usize>,
    /// Width and maximum height the slots were measured for. Rendering reuses
    /// it, so a picture comes out exactly as tall as the layout reserved.
    pub(crate) box_: (u16, u16),
    /// How this terminal draws pictures, decided once at startup.
    pub(crate) backend: crate::image::Backend,
    /// Pixel size of one cell, needed to scale pictures to whole cells.
    pub(crate) cell: crate::image::CellSize,
    /// Where a job is done: off this thread in a reader, and inside the call
    /// that queued it in a test, where a picture has to be in place before
    /// the frame that asked for it is asserted.
    executor: Executor,
    /// The blocks a job is out for. They are not in `rendered` yet, so they
    /// must not be asked for twice while the first answer is on its way, and
    /// the session waits on a clock rather than on a key while any of them
    /// stands — a finished picture has no keypress to wake the loop with.
    in_flight: std::collections::HashSet<usize>,
    /// Which questions the answers still in flight answer. Every change that
    /// empties `rendered` — another chapter, another room, another backend —
    /// raises it, so a picture rendered for what has just gone falls on the
    /// floor instead of landing in its place.
    generation: u64,
    /// Whether new decode jobs may go out. A picture decoded while the view
    /// scrolls is one decoded for a page the reader has already left, so the
    /// session says here once a frame whether the view has stood still long
    /// enough to be worth decoding for. On until something says otherwise, so
    /// a caller that never asks — every test — decodes immediately.
    decode_allowed: bool,
    /// True while wanted pictures wait on nothing but the view settling. The
    /// session runs its short clock on this, so the jobs go out the frame
    /// after the settle without a keypress to wake anything up.
    waiting: bool,
    /// The blocks within a screen either side of the view, as the last
    /// `render_visible` saw them. An answer for a block that has left this
    /// window outlived the reader's look at it, and its pixels are dropped
    /// rather than kept for a page gone by.
    nearby: Vec<usize>,
}

impl Pictures {
    /// A pipeline for one session: nothing measured, nothing decoded, and the
    /// block backend until the terminal says otherwise.
    pub(crate) fn new(cell: crate::image::CellSize) -> Self {
        Self {
            rendered: std::collections::HashMap::new(),
            slots: Vec::new(),
            unreadable: std::collections::HashSet::new(),
            box_: (0, 0),
            backend: crate::image::Backend::Quad,
            cell,
            executor: Executor::new(),
            in_flight: std::collections::HashSet::new(),
            generation: 0,
            decode_allowed: true,
            waiting: false,
            nearby: Vec::new(),
        }
    }

    /// The pixel size of one cell, which the chapter's pictures were measured
    /// against and which opening the next chapter has to keep.
    pub(crate) fn cell(&self) -> crate::image::CellSize {
        self.cell
    }

    /// How this terminal draws pictures.
    pub(crate) fn backend(&self) -> crate::image::Backend {
        self.backend
    }

    /// Sets how this terminal draws pictures. The pictures already decoded
    /// were drawn for the old backend's cells, so they go.
    pub(crate) fn set_backend(&mut self, backend: crate::image::Backend) {
        self.backend = backend;
        self.abandon_answers();
    }

    /// Says whether the view has stood still long enough for new decodes to
    /// start. The session sets it once a frame; the places that raise the
    /// generation lower it back to yes, because a chapter, a room or a
    /// backend that just changed owes its pictures at once.
    pub(crate) fn set_decode_allowed(&mut self, allowed: bool) {
        self.decode_allowed = allowed;
    }

    /// Clears the pictures drawn and turns its back on the ones still being
    /// drawn elsewhere: another chapter, another room or another backend has
    /// just taken their place, and the decoder is holding pictures asked for
    /// the one that went. Their answers have to fall on the floor rather than
    /// land in the new one, which is what the generation is for.
    fn abandon_answers(&mut self) {
        self.rendered.clear();
        self.generation = self.generation.wrapping_add(1);
        self.in_flight.clear();
        // The settle is about a view in motion; a chapter, a room or a
        // backend that has just arrived somewhere new owes its pictures on
        // the very frame that shows it, not a fifth of a second later.
        self.decode_allowed = true;
        self.waiting = false;
        self.nearby.clear();
    }

    /// Forgets a chapter's pictures: nothing drawn carries over to the next
    /// chapter, and its bytes have not lied yet, so they get their chance.
    /// The measured slots go too, and the room they were measured for with
    /// them — otherwise the next chapter measured in the same window would
    /// inherit this one's picture rows under its own blocks.
    pub(crate) fn forget_chapter(&mut self) {
        self.abandon_answers();
        self.slots.clear();
        self.unreadable.clear();
        self.box_ = (0, 0);
    }

    /// Works out how much room each of this chapter's pictures takes.
    ///
    /// Only the header of each entry is asked for — through the book's own
    /// size cache, so a picture's header is read once for the session however
    /// often the layout is rebuilt — and a picture whose header cannot be
    /// read or measured, or whose block already failed to decode, gets no
    /// slot, and the layout falls back to its alt text. A broken image must
    /// not cost the chapter.
    ///
    /// The room is remembered, and a re-measure for the room the slots
    /// already hold is no work at all: the slots still answer for the same
    /// box, and the pictures already decoded were decoded for it. Only a
    /// picture that has since proven undecodable gives its rows back — that
    /// is the one thing that can have changed while the room stood still.
    pub(crate) fn measure(
        &mut self,
        book: &mut Book,
        chapter: &Chapter,
        width: u16,
        max_rows: u16,
    ) {
        if width == 0 {
            self.abandon_answers();
            self.slots.clear();
            self.box_ = (0, 0);
            return;
        }
        // A picture belongs in the same column as the text, so it is measured
        // against the same width `layout_full` is given.
        let width = width.max(1);
        if (width, max_rows) == self.box_ {
            // The room has not moved: everything measured for it still holds,
            // and decoding it all again would only repeat the header reads.
            // The one thing that can have changed is a picture whose bytes
            // have since failed to decode — its rows go back to the text.
            self.slots
                .retain(|slot| !self.unreadable.contains(&slot.block));
            return;
        }
        // A real change of room: the old slots sized to the old room and the
        // old decoded pictures are drawn for it, so both go — and so do the
        // pictures on their way, which were asked for the room that just went.
        self.abandon_answers();
        self.slots.clear();
        self.box_ = (width, max_rows);

        let sources: Vec<(usize, String)> = chapter
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(index, block)| match &block.kind {
                crate::doc::BlockKind::Image { src: Some(src) } => Some((index, src.clone())),
                _ => None,
            })
            .collect();

        for (index, src) in sources {
            // A picture that already failed to decode reserves nothing and is
            // not tried again for this chapter.
            if self.unreadable.contains(&index) {
                continue;
            }
            // What the picture is in the book decides the box it is measured
            // for, long before its bytes are read.
            let reason = book.picture_reason(&src);
            // The size the book has already read, if it has read it: the same
            // header hiding the marks asked for a moment ago.
            let Some(size) = book.picture_size(&src) else {
                continue;
            };
            let (cols, rows) =
                crate::image::measure(size, width, max_rows, reason.role(), self.cell);
            if rows == 0 {
                continue;
            }
            self.slots.push(ImageSlot {
                block: index,
                src,
                rows,
                cols,
                reason,
            });
        }
    }

    /// How much room the layout must leave for each picture.
    pub(crate) fn placements(&self) -> Vec<layout::ImagePlacement> {
        self.slots
            .iter()
            .map(|slot| layout::ImagePlacement {
                block: slot.block,
                rows: slot.rows,
                cols: slot.cols,
            })
            .collect()
    }

    /// The pictures the view shows are decoded elsewhere and dropped when the
    /// view leaves them; what comes back is put in its place.
    ///
    /// Called before every draw, because scrolling changes which ones are
    /// needed. Decoding one picture costs milliseconds; decoding a chapter of
    /// them costs seconds, which is why only what is on screen is asked for —
    /// and why the asking is handed to a thread that is not the one drawing:
    /// a 1600x1200 PNG costs ten milliseconds the frame would not have.
    ///
    /// Answers whether a picture turned out to be undecodable, which is the
    /// one thing that has to reach the layout: the caller rebuilds it once,
    /// without the rows the broken picture was holding.
    ///
    /// While the view is moving, nothing goes out — only the wanting is
    /// remembered — because a picture decoded for a page the reader has
    /// already scrolled past is a picture thrown away.
    pub(crate) fn render_visible(
        &mut self,
        book: &mut Book,
        lines: &[Line],
        scroll: usize,
        view_height: u16,
    ) -> bool {
        // First, whatever the decoder finished while the last frame was up:
        // a picture that arrived belongs to this frame even when there is
        // nothing new below to ask for, and one that drew nothing must be
        // measured away by the build that hears about it.
        let mut broken = self.poll();
        let (width, max_rows) = self.box_;
        if width == 0 || self.slots.is_empty() {
            return broken;
        }
        let height = view_height as usize;
        // Pictures stand in reading order, so the blocks on screen come out
        // sorted and a small Vec answers every "is it in view" question
        // without building a hash set on every frame.
        let blocks_between = |from: usize, to: usize| -> Vec<usize> {
            let from = from.min(lines.len());
            let to = to.min(lines.len());
            let mut blocks: Vec<usize> = lines[from..to]
                .iter()
                .filter(|line| matches!(line.kind, layout::LineKind::Image { .. }))
                .map(|line| line.block)
                .collect();
            blocks.dedup();
            blocks
        };
        let visible = blocks_between(scroll, scroll + height);
        // Reading moves back as well as forward, and a picture just off the edge
        // is about to be wanted again. Keeping a screen either way spares the
        // decoding, and a handful of pictures is nothing to hold.
        let nearby = blocks_between(scroll.saturating_sub(height), scroll + height * 2);

        self.rendered.retain(|block, _| nearby.contains(block));
        // Kept for the answers still on their way: one that comes back for a
        // block outside this window has outlived the reader's look at it, and
        // `apply` frees its pixels rather than keep them for a page gone by.
        self.nearby = nearby;

        let wanted = |block: usize| {
            visible.contains(&block)
                && !self.rendered.contains_key(&block)
                // Already asked for and not answered yet: the rows the
                // picture will take are reserved either way, so the reader
                // waits a frame or two rather than pay twice for one picture.
                && !self.in_flight.contains(&block)
                // A block whose bytes have already failed this chapter is not
                // asked again before the layout has had its say.
                && !self.unreadable.contains(&block)
        };
        let wanted: Vec<usize> = self
            .slots
            .iter()
            .map(|slot| slot.block)
            .filter(|&block| wanted(block))
            .collect();
        if wanted.is_empty() {
            // Nothing new to ask for: the screen holds what it held a moment
            // ago, and the pictures that left are already dropped.
            self.waiting = false;
            return broken;
        }
        if !self.decode_allowed {
            // The view is still moving, and a picture decoded for it now is
            // one decoded for a page the reader has already scrolled past.
            // The wanting is remembered instead: the session keeps its short
            // clock running on it, and the jobs go out the frame after the
            // view settles.
            self.waiting = true;
            return broken;
        }
        self.waiting = false;

        // The slot's position is its Kitty id, so a picture keeps the same id
        // however often it leaves the screen and comes back.
        let pending: Vec<(u32, usize, String, crate::image::Reason)> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| wanted.contains(&slot.block))
            // Ids start at 1; 0 is reserved by the Kitty protocol.
            .map(|(id, slot)| (id as u32 + 1, slot.block, slot.src.clone(), slot.reason))
            .collect();

        // Every visible picture with nothing already standing in for it has
        // its bytes read here — the book belongs to this thread — and its job
        // handed over. An answer that comes back on the spot, which is only
        // the tests' executor, is put in its place at once: a block must not
        // hold rows for a picture that draws nothing, so a failure is
        // reported either way and the layout is rebuilt once without it.
        for (id, block, src, reason) in pending {
            // Drawing needs the whole picture, where measuring only needed
            // the header. These bytes are all the decoder is ever told
            // about, so it holds nothing of the app that could go stale.
            let bytes = book.read_binary(&src);
            let job = Job {
                generation: self.generation,
                block,
                id,
                reason,
                bytes,
                // The room, the backend and the cell this slot was measured
                // for, handed over with the bytes so the picture comes back
                // exactly as tall as the layout reserved for it, whatever has
                // happened here in the meantime.
                box_: (width, max_rows),
                backend: self.backend,
                cell: self.cell,
            };
            self.in_flight.insert(block);
            if let Some(answer) = self.executor.submit(job) {
                // The executor that answers on the spot — the tests' — puts
                // the picture in place before the frame that asked for it.
                broken |= self.apply(answer);
            }
        }
        broken
    }

    /// Takes in the pictures the decoder has finished, and says whether any of
    /// them tells the layout it was holding rows for nothing.
    ///
    /// Drained at the head of the frame's work: an answer that arrived while
    /// the last frame was being drawn belongs to this one, and a failure must
    /// be known before the room is measured so the rows it was holding go
    /// back to the text by the build that hears about it. Answers to questions
    /// that have since changed hands — another chapter, another room, another
    /// backend — are dropped by their generation without being looked at.
    pub(crate) fn poll(&mut self) -> bool {
        let answers = self.executor.take_answers();
        let mut broken = false;
        for answer in answers {
            broken |= self.apply(answer);
        }
        broken
    }

    /// Whether the session owes the view a frame on its short clock: a
    /// picture still being decoded off this thread, or wanted pictures
    /// waiting only for the view to settle. The clock runs on either, so a
    /// finished picture is drawn while the reader still looks at the place
    /// it will appear, and a settle that has run out asks for the jobs —
    /// neither owes a keypress.
    pub(crate) fn jobs_outstanding(&self) -> bool {
        !self.in_flight.is_empty() || self.waiting
    }

    /// One finished decode, in its place or out of the layout — and whether
    /// the layout has to be rebuilt without the rows a failure was holding.
    ///
    /// An answer to an older generation belongs to a room, a chapter or a
    /// backend that has since gone: nobody is waiting for it, so it is not
    /// allowed near the pictures the reader is looking at now.
    fn apply(&mut self, (generation, block, result): Answer) -> bool {
        if generation != self.generation {
            return false;
        }
        self.in_flight.remove(&block);
        match result {
            Ok(rendered) if rendered.height() > 0 => {
                // The picture is only worth its pixels near the view: one
                // that took longer than the reader's look at it has scrolled
                // away, and keeping it would hold a screen gone by.
                if self.nearby.contains(&block) {
                    self.rendered.insert(block, rendered);
                }
                false
            }
            // The header measured but the bytes do not decode — a lying
            // or truncated header. The block stops reserving rows for a
            // picture that will never appear, and this chapter does not
            // ask about these bytes again.
            _ => {
                self.rendered.remove(&block);
                self.unreadable.insert(block);
                true
            }
        }
    }

    /// Takes the decoder off the spot for a test that wants to see the round
    /// trip itself: jobs now wait for a `poll` to run them instead of
    /// finishing inside the call that queued them, so a picture can be held
    /// in the state a reader spends most of its time in — asked for, not yet
    /// answered — and its answers brought in by hand.
    #[cfg(test)]
    pub(crate) fn defer_jobs(&mut self) {
        self.executor = Executor::Queued(Vec::new());
    }

    /// The picture of a block, once rendered.
    pub(crate) fn at(&self, block: usize) -> Option<&crate::image::Rendered> {
        self.rendered.get(&block)
    }

    /// Whether a block's picture is still on its way: the layout has
    /// reserved its rows, and the view holds a placeholder in them until the
    /// decoder answers — never for a picture that is already drawn, that has
    /// proven undecodable (its alt line says so), or that never measured.
    pub(crate) fn pending(&self, block: usize) -> bool {
        self.slots.iter().any(|slot| slot.block == block)
            && !self.rendered.contains_key(&block)
            && !self.unreadable.contains(&block)
    }

    /// True when a picture needs a pixel protocol to appear.
    pub(crate) fn has_pixels(&self) -> bool {
        matches!(
            self.backend,
            crate::image::Backend::Kitty | crate::image::Backend::Sixel
        ) && !self.rendered.is_empty()
    }
}

/// One picture's decode, as it travels to whoever does it off this thread.
///
/// Everything the decoder needs is in here, and nothing of the app is: it is
/// handed the bytes and the room they were measured for, and answers with a
/// picture or a failure. It never sees the book, the chapter or the view, so
/// there is nothing it could hold on to that has since gone.
struct Job {
    /// The questions this answer will still be an answer to. Another chapter,
    /// another room or another backend raises the number, and an answer under
    /// an older one belongs to nobody.
    generation: u64,
    /// The block the picture stands in.
    block: usize,
    /// The slot's position, as its Kitty id: a picture keeps the same id
    /// however often it leaves the screen and comes back.
    id: u32,
    reason: crate::image::Reason,
    /// The whole picture, read on the thread the book lives on. Measuring
    /// needed only the header; drawing needs every byte, and these are all the
    /// decoder is given.
    bytes: anyhow::Result<Vec<u8>>,
    /// The room the slot was measured for, and how this terminal draws
    /// pictures, so the answer comes out the size the layout reserved even
    /// though the picture went out before any of it was looked at again.
    box_: (u16, u16),
    backend: crate::image::Backend,
    cell: crate::image::CellSize,
}

/// A finished decode: which questions it answers, which block it belongs to,
/// and what came of it — the picture, or the failure the layout hears about.
type Answer = (u64, usize, anyhow::Result<crate::image::Rendered>);

impl Job {
    /// Does the work, wherever the executor says it should happen: on the
    /// drawing thread's own body in a test, on a thread of its own in a
    /// reader. It asks nothing but the bytes it was handed.
    fn run(self) -> Answer {
        let Job {
            generation,
            block,
            id,
            reason,
            bytes,
            box_,
            backend,
            cell,
        } = self;
        let result = bytes.and_then(|bytes| {
            crate::image::render(&bytes, box_.0, box_.1, reason.role(), backend, id, cell)
        });
        (generation, block, result)
    }
}

/// Where a job is done.
enum Executor {
    /// The executor a test gets from `Pictures::new`: `submit` does the work
    /// and hands the answer straight back, so every picture is in place before
    /// the frame that asked for it is drawn, and a test that asserts a page
    /// sees the reader exactly as it was before decoding moved off-thread.
    Inline,
    /// The reader's: one thread, jobs in through its sender, answers back
    /// through the receiver. Dropped with the pictures, which closes the job
    /// channel and lets the thread end of its own accord — nothing joins it,
    /// because nothing waits for a picture it has stopped wanting.
    Worker {
        jobs: std::sync::mpsc::Sender<Job>,
        answers: std::sync::mpsc::Receiver<Answer>,
    },
    /// The executor for the round trip itself: jobs wait in a queue until a
    /// poll runs them, so a test can hold a picture in the state a reader
    /// spends most of its time in — asked for, not yet answered.
    #[cfg(test)]
    Queued(Vec<Job>),
}

impl Executor {
    /// The executor the reader uses: a thread of its own, and the inline one
    /// under `cfg!(test)` where a picture has to be in place the moment the
    /// frame that asked for it is prepared.
    fn new() -> Self {
        if cfg!(test) {
            Executor::Inline
        } else {
            Self::worker()
        }
    }

    /// Starts the decoder: it does nothing but take jobs, render them and send
    /// the answers back, and it stops when the pictures are dropped and the
    /// job channel closes with them.
    fn worker() -> Self {
        let (jobs, waiting) = std::sync::mpsc::channel::<Job>();
        let (answers, finished) = std::sync::mpsc::channel::<Answer>();
        std::thread::spawn(move || {
            while let Ok(job) = waiting.recv() {
                // A closed receiver means the reader has gone: the answer is
                // for no one, and the send is how the thread finds out and
                // leaves.
                if answers.send(job.run()).is_err() {
                    break;
                }
            }
        });
        Executor::Worker {
            jobs,
            answers: finished,
        }
    }

    /// Hands a job over. `Some` back means the answer was here already — only
    /// the inline executor answers on the spot — and the caller puts it in
    /// its place like any other.
    fn submit(&mut self, job: Job) -> Option<Answer> {
        match self {
            Executor::Inline => Some(job.run()),
            Executor::Worker { jobs, .. } => {
                // A decoder that has gone takes the picture with it, not the
                // reader: a job that cannot be sent was never asked for.
                let _ = jobs.send(job);
                None
            }
            #[cfg(test)]
            Executor::Queued(queue) => {
                queue.push(job);
                None
            }
        }
    }

    /// The answers that are ready, and none that are not: the point is never
    /// to wait here. Under `Queued`, running the queue is the poll a test
    /// drives by hand.
    fn take_answers(&mut self) -> Vec<Answer> {
        match self {
            Executor::Inline => Vec::new(),
            Executor::Worker { answers, .. } => {
                let mut ready = Vec::new();
                while let Ok(answer) = answers.try_recv() {
                    ready.push(answer);
                }
                ready
            }
            #[cfg(test)]
            Executor::Queued(queue) => queue.drain(..).map(Job::run).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{Backend, CellSize, Role};

    /// A book whose first chapter holds two pictures of different shapes and
    /// whose second holds none — so a chapter change is visible in the slots
    /// rather than hidden by another set of pictures just like them.
    fn art_book(name: &str) -> crate::testkit::TempBook {
        crate::testkit::TempBook::with(
            name,
            "Art",
            &[
                r#"<p>before</p><img src="wide.png" alt="wide"/><p>mid</p><img src="tall.png" alt="tall"/><p>after</p>"#,
                "<p>a chapter with no pictures at all</p>",
            ],
            &[
                (
                    "OEBPS/wide.png",
                    &crate::testkit::png(200, 60, [10, 20, 30, 255]),
                ),
                (
                    "OEBPS/tall.png",
                    &crate::testkit::png(60, 200, [40, 50, 60, 255]),
                ),
            ],
        )
    }

    /// What the slots hold, as numbers a test can compare: which block, and
    /// how much room it takes.
    fn measured(pictures: &Pictures) -> Vec<(usize, u16, u16)> {
        pictures
            .slots
            .iter()
            .map(|slot| (slot.block, slot.cols, slot.rows))
            .collect()
    }

    #[test]
    fn a_second_measure_of_the_same_room_changes_nothing() {
        // A resize is not the only thing that rebuilds the layout — a picture
        // that fails to decode does it too, and the rebuild lands on the same
        // room the slots already hold. Re-measuring there used to clear every
        // decoded picture and read every header again: the visible art was
        // decoded a second time and each header cost 210 microseconds. For
        // the same room, none of that work may happen.
        let path = art_book("pictures-same-room");
        let cell = CellSize::default();
        let mut book = Book::open(&path).unwrap();
        let chapter = crate::epub::marks::load_chapter(&mut book, 0, cell);
        let mut pictures = Pictures::new(cell);

        pictures.measure(&mut book, &chapter, 40, 12);
        assert_eq!(
            measured(&pictures).len(),
            2,
            "both pictures of the chapter measured"
        );
        // A picture the view has already decoded for this very room.
        let decoded = crate::image::render(
            &crate::testkit::png(200, 60, [10, 20, 30, 255]),
            40,
            12,
            Role::Keep,
            Backend::Quad,
            1,
            cell,
        )
        .unwrap();
        pictures.rendered.insert(1, decoded);
        let before = measured(&pictures);
        let reads = book.header_reads();

        pictures.measure(&mut book, &chapter, 40, 12);

        assert_eq!(
            measured(&pictures),
            before,
            "the slots were rebuilt for the room they were already measured for"
        );
        assert!(
            pictures.rendered.contains_key(&1),
            "the decoded picture was thrown away and would be decoded again"
        );
        assert_eq!(
            book.header_reads(),
            reads,
            "a picture header was read for an answer the cache already held"
        );
    }

    #[test]
    fn a_new_room_and_a_new_chapter_are_measured_again() {
        // The other half of the bargain: a room that really changed must
        // re-measure — the old slots sized to the old room — and a chapter
        // that follows another must not inherit its picture rows, or a text
        // chapter would open with the last chapter's pictures' lines.
        let path = art_book("pictures-new-room");
        let cell = CellSize::default();
        let mut book = Book::open(&path).unwrap();
        let chapter = crate::epub::marks::load_chapter(&mut book, 0, cell);
        let mut pictures = Pictures::new(cell);

        pictures.measure(&mut book, &chapter, 40, 12);
        let wide_room = measured(&pictures);
        pictures.measure(&mut book, &chapter, 20, 12);
        assert_ne!(
            measured(&pictures),
            wide_room,
            "the pictures were not measured for the narrower room"
        );
        assert!(
            pictures.rendered.is_empty(),
            "pictures drawn for the old room are still standing in for the new one"
        );

        // The chapter changes: everything the last one measured goes, so the
        // same room cannot answer with the slots it answered with before.
        pictures.rendered.insert(
            1,
            crate::image::render(
                &crate::testkit::png(60, 200, [40, 50, 60, 255]),
                20,
                12,
                Role::Keep,
                Backend::Quad,
                1,
                cell,
            )
            .unwrap(),
        );
        pictures.forget_chapter();
        assert!(pictures.slots.is_empty(), "the last chapter's slots stayed");
        assert!(
            pictures.rendered.is_empty(),
            "the last chapter's pictures stayed"
        );
        assert_eq!(
            pictures.box_,
            (0, 0),
            "the last chapter's room was remembered for the next one"
        );

        let second = crate::epub::marks::load_chapter(&mut book, 1, cell);
        pictures.measure(&mut book, &second, 20, 12);
        assert!(
            pictures.slots.is_empty(),
            "a chapter with no pictures was given the last chapter's picture rows"
        );
    }

    /// A book whose one chapter runs long: a drawing at the top, a picture
    /// whose bytes lie a screen below it, and text past that — so the view
    /// can leave one picture far behind while it stands on the other.
    fn long_book(name: &str) -> crate::testkit::TempBook {
        let filler =
            "<p>a paragraph of the filler this chapter is set with, two rows wide here</p>";
        let body = format!(
            r#"<img src="wide.png" alt="wide"/>{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}<img src="bad.png" alt="liar"/>{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}{f}"#,
            f = filler,
        );
        crate::testkit::TempBook::with(
            name,
            "Long",
            &[&body],
            &[
                (
                    "OEBPS/content.opf",
                    &crate::testkit::opf(
                        "Long",
                        r#"<item id="wide" href="wide.png" media-type="image/png"/>
  <item id="bad" href="bad.png" media-type="image/png"/>"#,
                    ),
                ),
                (
                    "OEBPS/wide.png",
                    &crate::testkit::png(200, 60, [10, 20, 30, 255]),
                ),
                ("OEBPS/bad.png", &crate::testkit::broken_png(200, 60)),
            ],
        )
    }

    /// The first and last line a block holds in the layout.
    fn rows_of(lines: &[Line], block: usize) -> (usize, usize) {
        let mut rows = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.block == block);
        let first = rows.next().expect("the block has rows").0;
        (first, rows.next_back().map_or(first, |(i, _)| i))
    }

    /// Takes the answers back out of a deferred executor, so a test can hold
    /// them while the view moves and let them arrive where it chooses.
    fn hold_answers(pictures: &mut Pictures) -> Vec<Job> {
        match &mut pictures.executor {
            Executor::Queued(queue) => std::mem::take(queue),
            _ => panic!("deferred jobs wait in a queue"),
        }
    }

    #[test]
    fn a_view_that_has_not_settled_waits_and_says_it_is_busy() {
        // A picture decoded while the view scrolls is one decoded for a page
        // the reader has already left. While decoding is not allowed no job
        // may go out — but the wait has to count as busy, or the session
        // would block on a keypress and the jobs would never be asked for.
        let path = art_book("pictures-settle");
        let cell = CellSize::default();
        let mut book = Book::open(&path).unwrap();
        let chapter = crate::epub::marks::load_chapter(&mut book, 0, cell);
        let mut pictures = Pictures::new(cell);
        pictures.defer_jobs();
        pictures.measure(&mut book, &chapter, 40, 24);
        let lines = crate::layout::layout_full(&chapter, 40, &pictures.placements());

        pictures.set_decode_allowed(false);
        assert!(
            !pictures.render_visible(&mut book, &lines, 0, 24),
            "nothing ran, so nothing could break"
        );
        assert!(
            pictures.in_flight.is_empty(),
            "no job goes out while the view is moving"
        );
        assert!(
            pictures.jobs_outstanding(),
            "the waiting is work to do: the loop must keep its clock running"
        );

        // The view settles: one job per wanted picture, and no more.
        pictures.set_decode_allowed(true);
        pictures.render_visible(&mut book, &lines, 0, 24);
        assert_eq!(pictures.in_flight.len(), 2, "both pictures asked for");
        assert!(
            matches!(&pictures.executor, Executor::Queued(jobs) if jobs.len() == 2),
            "one job each, not two"
        );

        // The answers come in on the next frame, which asks for nothing new:
        // the pictures it wants are the very ones that arrived.
        pictures.render_visible(&mut book, &lines, 0, 24);
        assert!(
            matches!(&pictures.executor, Executor::Queued(jobs) if jobs.is_empty()),
            "a second frame asks for nothing twice"
        );
        assert_eq!(
            pictures.rendered.len(),
            2,
            "the answers landed on the frame that ran them"
        );
        assert!(
            !pictures.jobs_outstanding(),
            "nothing in flight and nothing waiting: no clock to keep"
        );
    }

    #[test]
    fn a_picture_for_a_view_the_reader_has_left_is_dropped() {
        // The decode outlives the reader's look at it: by the time the
        // answer comes back, the view has moved a screen away. Its pixels
        // are dropped rather than kept for a page gone by — while an answer
        // the view is still near lands as it always did.
        let path = long_book("pictures-drop");
        let cell = CellSize::default();
        let mut book = Book::open(&path).unwrap();
        let chapter = crate::epub::marks::load_chapter(&mut book, 0, cell);
        let mut pictures = Pictures::new(cell);
        pictures.defer_jobs();
        pictures.measure(&mut book, &chapter, 40, 12);
        let lines = crate::layout::layout_full(&chapter, 40, &pictures.placements());
        let wide = pictures.slots[0].block;

        // The top of the chapter: the drawing goes out and waits in the queue.
        pictures.render_visible(&mut book, &lines, 0, 12);
        assert_eq!(pictures.in_flight.len(), 1, "the one picture in view");

        // The answer is held back while the view moves past the picture — to
        // a place where nothing is wanted, so nothing new goes out either.
        let held = hold_answers(&mut pictures);
        let away = (rows_of(&lines, wide).1 + 13).min(lines.len() - 12);
        assert!(away > rows_of(&lines, wide).1 + 12, "the view left the picture behind");
        pictures.render_visible(&mut book, &lines, away, 12);
        assert!(pictures.rendered.is_empty(), "nothing has been answered yet");

        // The answer arrives for a view it no longer belongs to.
        pictures.executor = Executor::Queued(held);
        assert!(
            !pictures.poll(),
            "a dropped picture tells the layout nothing"
        );
        assert!(
            pictures.rendered.is_empty(),
            "the picture for the page gone by was installed after all"
        );
        assert!(
            !pictures.jobs_outstanding(),
            "the stale answer let go of its block"
        );

        // Back where the picture is wanted, the same shape of answer
        // installs rather than drops.
        pictures.render_visible(&mut book, &lines, 0, 12);
        let held = hold_answers(&mut pictures);
        assert_eq!(
            held.len(),
            1,
            "asked for once more, since none was ever installed"
        );
        pictures.executor = Executor::Queued(held);
        pictures.poll();
        assert!(
            pictures.at(wide).is_some(),
            "the wanted picture is installed"
        );
    }

    #[test]
    fn a_failure_from_a_block_that_left_the_view_still_lands() {
        // A picture whose bytes lie must give its rows back wherever the view
        // has got to: the layout is rebuilt once without them, or the chapter
        // goes on holding a hole for a picture that will never appear.
        let path = long_book("pictures-departed-failure");
        let cell = CellSize::default();
        let mut book = Book::open(&path).unwrap();
        let chapter = crate::epub::marks::load_chapter(&mut book, 0, cell);
        let mut pictures = Pictures::new(cell);
        pictures.defer_jobs();
        pictures.measure(&mut book, &chapter, 40, 12);
        let lines = crate::layout::layout_full(&chapter, 40, &pictures.placements());
        let bad = pictures.slots[1].block;

        // The view stands on the liar, and its job goes out.
        let (_, last) = rows_of(&lines, bad);
        pictures.render_visible(&mut book, &lines, rows_of(&lines, bad).0, 12);
        assert!(pictures.in_flight.contains(&bad), "the liar was asked for");

        // The reader scrolls past it before the answer comes back.
        let held = hold_answers(&mut pictures);
        let away = (last + 13).min(lines.len() - 12);
        assert!(
            away > last + 12,
            "the failed block leaves the nearby window"
        );
        pictures.render_visible(&mut book, &lines, away, 12);
        pictures.executor = Executor::Queued(held);
        assert!(
            pictures.poll(),
            "the layout still hears that rows were held for nothing"
        );
        assert!(
            pictures.unreadable.contains(&bad),
            "the failure lands where failures land"
        );
        assert!(
            pictures.rendered.is_empty(),
            "and nothing was installed for the page gone by"
        );
        assert!(!pictures.jobs_outstanding(), "and no job is left standing");
    }
}
