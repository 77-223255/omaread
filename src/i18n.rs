//! The words on screen, in the language the machine is set to.
//!
//! Every string a reader sees is written in English where it is used and looked
//! up through [`t`] on the way out. English is the key, so there is no second
//! table to keep in step and nothing to go stale: a string without a translation
//! is simply English, which is what an untranslated string should look like.
//!
//! The language comes from the environment the way every other program's does —
//! `LC_ALL`, then `LC_MESSAGES`, then `LANG`, and the part before the first `_`
//! or `.` is the language. Anything unrecognised means English, so a machine set
//! to a language nobody has translated yet reads the same as it always did.
//!
//! Only what a person reads is here. What a script reads — `list --json`, the
//! table `list` prints, scan progress — stays English, because
//! that output is an interface and it should not change with a shell variable.

use std::sync::OnceLock;

/// A language the reader has words for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    /// Reads the language out of the usual variables.
    pub fn detect() -> Self {
        for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(name)
                && let Some(lang) = Self::from_locale(&value) {
                    return lang;
                }
        }
        Self::En
    }

    /// Reads a locale string such as `zh_CN.UTF-8`, `en_GB` or `C`.
    ///
    /// A value that names no language — empty, `C`, `POSIX`, anything without
    /// letters — is not a language and is skipped, so the next variable gets its
    /// turn. That matters because a shell sets `LC_ALL` to the empty string to
    /// mean "nothing in particular", and treating that as English would hide the
    /// language the user actually chose in `LANG`.
    pub fn from_locale(value: &str) -> Option<Self> {
        let language = value
            .split(['.', '@'])
            .next()?
            .split(['_', '-'])
            .next()?
            .trim()
            .to_ascii_lowercase();
        match language.as_str() {
            "" | "c" | "posix" => None,
            "zh" => Some(Self::Zh),
            _ => Some(Self::En),
        }
    }
}

fn lang() -> Lang {
    static LANG: OnceLock<Lang> = OnceLock::new();
    *LANG.get_or_init(Lang::detect)
}

/// Every word the reader can say in Chinese, English text first.
///
/// A table rather than a match so that a test can walk it: a lookup whose key is
/// missing would quietly show English, which is the one failure of a translation
/// nobody notices.
static ZH: &[(&str, &str)] = &[
    (
        "cursor mode: Enter follows a link, i leaves",
        "光标模式：Enter 跟随链接，i 退出",
    ),
    ("search cleared", "已清除搜索"),
    ("no link here", "此处没有链接"),
    ("nowhere to go back to", "没有可返回的位置"),
    (
        "nothing searched yet: / starts a search",
        "还没搜索过：按 / 开始",
    ),
    ("end of book", "已是全书末尾"),
    ("start of book", "已是全书开头"),
    ("cannot save position: {}", "无法保存阅读位置：{}"),
    ("chapter not found: {}", "找不到章节：{}"),
    ("external link: {}", "外部链接：{}"),
    (
        "target is not in the reading order: {}",
        "目标不在阅读顺序中：{}",
    ),
    ("no more matches for {}", "没有更多匹配：{}"),
    ("link: {}  ·  Enter follows", "链接：{}  ·  Enter 跟随"),
    (" Contents ", " 目录 "),
    (" Keys - any key closes ", " 按键 - 任意键关闭 "),
    ("Library", "书库"),
    ("Enter opens  ·  q leaves", "Enter 打开  ·  q 离开"),
    ("by {}  ·  ? for keys", "按{}  ·  ? 查按键"),
    ("{} books", "{} 本书"),
    ("{} book", "{} 本书"),
    ("{} of {} books  ·  filter {}", "{} / {} 本书  ·  筛选 {}"),
    ("{} of {} book  ·  filter {}", "{} / {} 本书  ·  筛选 {}"),
    ("sorted by {}", "已按{}排序"),
    ("no book matches {}", "没有匹配的书：{}"),
    ("  no book matches {}", "  没有匹配的书：{}"),
    ("file is gone: {}", "文件不存在：{}"),
    ("file is gone", "文件不存在"),
    ("no file recorded for this book", "这本书没有记录文件路径"),
    (
        "filter by title, author, series or tag",
        "按标题、作者、系列或标签筛选",
    ),
    ("filter cleared", "已清除筛选"),
    ("clear the filter", "清除筛选"),
    ("open the book", "打开这本书"),
    ("cycle the order: title, author", "切换排序：标题、作者"),
    (
        "sort by anything: type a criterion for the model",
        "任意排序：输入一句标准，交给模型",
    ),
    ("Super sort", "超级排序"),
    (
        "Sort the books by anything the model can judge:",
        "输入一句标准，让模型给每本书打分：",
    ),
    ("Enter sorts  ·  Esc cancels", "Enter 排序  ·  Esc 取消"),
    ("asking the decision model ...", "正在询问决策模型……"),
    ("no decision model is configured", "尚未配置决策模型"),
    (
        "no decision model: the plain author order",
        "没有决策模型：用普通的作者排序",
    ),
    (
        "the decision model could not answer: {}",
        "决策模型无法回答：{}",
    ),
    ("down, up", "下、上"),
    ("first, last", "首、尾"),
    ("page down, up", "翻页下、上"),
    ("quit", "退出"),
    ("title", "标题"),
    ("author", "作者"),
    ("super", "超级"),
    ("series", "系列"),
    ("{} hits for {}  ·  {}", "{} 条结果：{}  ·  {}"),
    ("{} hit for {}  ·  {}", "{} 条结果：{}  ·  {}"),
    ("qmd index", "qmd 索引"),
    ("read directly", "直接读取"),
    ("just now", "刚刚"),
    ("{} min ago", "{} 分钟前"),
    ("{} h ago", "{} 小时前"),
    ("yesterday", "昨天"),
    ("{} days ago", "{} 天前"),
    ("{} months ago", "{} 个月前"),
    ("{} years ago", "{} 年前"),
    ("Reading", "阅读"),
    ("Cursor", "光标"),
    ("Contents", "目录"),
    ("line down, up", "上下移动一行"),
    ("half a page", "半页"),
    ("start, end of chapter", "章节开头、末尾"),
    ("next chapter", "下一章"),
    ("previous chapter", "上一章"),
    ("contents", "目录"),
    ("search the book", "在本书内搜索"),
    ("next, previous match", "下一个、上一个匹配"),
    (
        "cursor mode, with a cursor in the text",
        "光标模式，文字中出现光标",
    ),
    ("clear the search", "清除搜索"),
    ("back to the library", "返回书库"),
    (
        "move by character, word, to line edge",
        "按字符、词、行首行尾移动",
    ),
    ("move by line, to chapter edges", "按行、章节首尾移动"),
    ("follow the link under the cursor", "跟随光标处的链接"),
    ("back out of followed links", "从跟随的链接返回"),
    ("search, next match, previous match", "搜索、下一个、上一个"),
    ("leave the cursor", "退出光标"),
    ("move the cursor", "移动光标"),
    ("open the chapter", "打开章节"),
    ("close", "关闭"),
];

/// The translation of `en`, when there is one.
pub fn translated(en: &str) -> Option<&'static str> {
    ZH.iter().find(|(key, _)| *key == en).map(|(_, text)| *text)
}

/// The reader's words, in the language this machine is set to.
pub fn t(en: &'static str) -> &'static str {
    match lang() {
        Lang::En => en,
        Lang::Zh => translated(en).unwrap_or(en),
    }
}

/// Fills a translated template, one `{}` per argument.
///
/// `format!` insists on a literal, and a translated template is not one, so the
/// substitution happens here instead. A template with fewer placeholders than
/// arguments keeps what is left over, which is what a half-finished translation
/// looks like rather than a panic.
pub fn fill(template: &'static str, args: &[&dyn std::fmt::Display]) -> String {
    let mut rest = t(template);
    let mut out = String::with_capacity(rest.len() + 16);
    for arg in args {
        match rest.split_once("{}") {
            Some((head, tail)) => {
                out.push_str(head);
                out.push_str(&arg.to_string());
                rest = tail;
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_locale_gives_up_its_language() {
        assert_eq!(Lang::from_locale("zh_CN.UTF-8"), Some(Lang::Zh));
        assert_eq!(Lang::from_locale("zh_TW"), Some(Lang::Zh));
        assert_eq!(Lang::from_locale("zh-Hans-CN"), Some(Lang::Zh));
        assert_eq!(Lang::from_locale("en_US.UTF-8"), Some(Lang::En));
        assert_eq!(Lang::from_locale("de_DE"), Some(Lang::En));
        assert_eq!(Lang::from_locale(""), None);
        assert_eq!(Lang::from_locale("C"), None);
        assert_eq!(Lang::from_locale("POSIX"), None);
        assert_eq!(Lang::from_locale("C.UTF-8"), None);
    }

    #[test]
    fn a_string_nobody_translated_stays_as_it_was() {
        // The lookup is the fallback, not an error: a string added to the reader
        // and not yet translated reads exactly as it was written.
        assert_eq!(
            t("a string that is not in the table"),
            "a string that is not in the table"
        );
    }

    /// Every lookup in the reader, found in its own source.
    ///
    /// A key that is not in the table does not fail anything: the string simply
    /// stays English. That is the right fallback and the wrong thing to notice by
    /// eye, so it is checked here instead. Retrieval is by scanning the sources
    /// as text, which is the only way to see a `t("...")` call.
    fn keys_looked_up(source: &str) -> Vec<String> {
        let mut keys = Vec::new();
        for (index, _) in source.match_indices("i18n::") {
            let rest = &source[index + "i18n::".len()..];
            let rest = match rest
                .strip_prefix("t(")
                .or_else(|| rest.strip_prefix("fill("))
            {
                Some(rest) => rest,
                None => continue,
            };
            if let Some(inner) = rest.strip_prefix('"')
                && let Some(end) = inner.find('"') {
                    keys.push(inner[..end].to_string());
                }
        }
        keys
    }

    #[test]
    fn everything_the_reader_displays_is_in_the_table() {
        let sources = [
            include_str!("app.rs"),
            include_str!("ui.rs"),
            include_str!("shelf.rs"),
            include_str!("main.rs"),
        ];
        let mut missing = Vec::new();
        let mut checked = 0;
        for source in sources {
            for key in keys_looked_up(source) {
                checked += 1;
                if translated(&key).is_none() {
                    missing.push(key);
                }
            }
        }
        // The order is looked up with a value the source scan cannot see, so it
        // is checked against the things that produce the values.
        for order in [
            crate::library::Order::Title,
            crate::library::Order::Author,
            crate::library::Order::Super,
        ] {
            checked += 1;
            if translated(order.label()).is_none() {
                missing.push(order.label().to_string());
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "these are looked up but not translated: {missing:?}"
        );
        // A floor, not an exact count: the number moves with the reader and
        // with how the calls happen to be wrapped, but a scan that suddenly
        // finds nothing would pass the check above unnoticed.
        assert!(
            checked > 20,
            "only {checked} lookups found; the scan is looking in the wrong place"
        );
    }
}
