// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A cheap, brace-aware scan of one Rust source file: where each `fn` starts and ends and how many
//! parameters it takes ([ADR-0170](../../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md)).
//!
//! Not a parser. Comments, strings, raw strings and character literals are blanked first, so a
//! brace or a comma inside one is never counted; what remains is matched by bracket depth. That is
//! enough for line and parameter counts, and it costs no dependency.

/// One function found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSpan {
    /// The function's name.
    pub name: String,
    /// Its name inside what encloses it: `Edge::open_bill` for a method, `<Bill as Display>::fmt`
    /// for a trait method, `tests::helper` inside a module, `outer::inner` for a nested function.
    /// Stable when code above it moves, unlike a line number.
    pub qualified: String,
    /// The 1-based line its `fn` keyword is on.
    pub line: usize,
    /// Lines from the `fn` keyword to the closing brace, both included.
    pub lines: usize,
    /// Parameters, `self` included, as clippy's `too_many_arguments` counts them.
    pub params: usize,
}

/// The source with every comment, string and character literal replaced by spaces, keeping each
/// newline, so byte offsets and line numbers still match the original.
pub fn blank_literals(source: &str) -> Vec<u8> {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut at = 0;
    while at < bytes.len() {
        let end = literal_end(bytes, at);
        if end > at {
            for byte in out.iter_mut().take(end).skip(at) {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
            at = end;
        } else {
            at += 1;
        }
    }
    out
}

/// Where the comment or literal starting at `at` ends, or `at` itself when none starts there.
fn literal_end(bytes: &[u8], at: usize) -> usize {
    let next = |offset: usize| bytes.get(at + offset).copied();
    let ident_before = at > 0 && bytes.get(at - 1).is_some_and(|b| is_ident(*b));
    match (next(0), next(1)) {
        (Some(b'/'), Some(b'/')) => line_end(bytes, at),
        (Some(b'/'), Some(b'*')) => block_comment_end(bytes, at),
        (Some(b'"'), _) => string_end(bytes, at + 1),
        (Some(b'r'), Some(b'"' | b'#')) if !ident_before => raw_string_end(bytes, at + 1),
        (Some(b'b'), Some(b'r')) if !ident_before && matches!(next(2), Some(b'"' | b'#')) => {
            raw_string_end(bytes, at + 2)
        }
        (Some(b'\''), _) => char_literal_end(bytes, at),
        _ => at,
    }
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn line_end(bytes: &[u8], at: usize) -> usize {
    bytes[at..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(bytes.len(), |offset| at + offset)
}

/// Block comments nest in Rust.
fn block_comment_end(bytes: &[u8], at: usize) -> usize {
    let mut depth = 0usize;
    let mut index = at;
    while index + 1 < bytes.len() {
        match (bytes[index], bytes[index + 1]) {
            (b'/', b'*') => {
                depth += 1;
                index += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    return index;
                }
            }
            _ => index += 1,
        }
    }
    bytes.len()
}

/// `from` is just past the opening quote.
fn string_end(bytes: &[u8], from: usize) -> usize {
    let mut index = from;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

/// `from` is at the first `#` or the quote after the `r`.
fn raw_string_end(bytes: &[u8], from: usize) -> usize {
    let hashes = bytes[from..].iter().take_while(|b| **b == b'#').count();
    let quote = from + hashes;
    if bytes.get(quote) != Some(&b'"') {
        return from;
    }
    let mut index = quote + 1;
    while index < bytes.len() {
        if bytes[index] == b'"' && bytes[index + 1..].iter().take(hashes).all(|b| *b == b'#') {
            let closing = index + 1 + hashes;
            if closing <= bytes.len() {
                return closing;
            }
        }
        index += 1;
    }
    bytes.len()
}

/// A character literal (`'a'`, `'\n'`, `'é'`), or `at` for a lifetime (`'a`).
fn char_literal_end(bytes: &[u8], at: usize) -> usize {
    if bytes.get(at + 1) == Some(&b'\\') {
        let close = bytes[at + 2..].iter().position(|b| *b == b'\'');
        return close.map_or(at, |offset| at + 3 + offset);
    }
    // One character, which may take up to four bytes, then the closing quote. The width comes
    // from the UTF-8 lead byte, so a lifetime costs no decoding of the rest of the file.
    let width = match bytes.get(at + 1) {
        Some(lead) if *lead >= 0xF0 => 4,
        Some(lead) if *lead >= 0xE0 => 3,
        Some(lead) if *lead >= 0xC0 => 2,
        _ => 1,
    };
    if bytes.get(at + 1 + width) == Some(&b'\'') {
        at + 2 + width
    } else {
        at
    }
}

/// Every function with a body in `source`, in order of appearance.
pub fn functions(source: &str) -> Vec<FunctionSpan> {
    let code = blank_literals(source);
    let newlines: Vec<usize> = code
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
        .collect();
    let mut scopes = scopes(&code);
    let mut found = Vec::new();
    let mut bodies = Vec::new();
    let mut at = 0;
    while let Some(offset) = find_keyword(&code, at, b"fn") {
        at = offset + 2;
        if let Some((span, body)) = function_at(&code, &newlines, offset) {
            bodies.push((offset, body, span.name.clone()));
            found.push((offset, span));
        }
    }
    scopes.extend(bodies);
    scopes.sort_by_key(|(start, _, _)| *start);
    found
        .into_iter()
        .map(|(offset, mut span)| {
            let mut path: Vec<&str> = scopes
                .iter()
                .filter(|(start, end, _)| *start < offset && offset < *end)
                .map(|(_, _, label)| label.as_str())
                .collect();
            path.push(&span.name);
            span.qualified = path.join("::");
            span
        })
        .collect()
}

/// What `#[cfg(test)]` gates in one file: the lines of each gated item, and the out-of-line
/// modules it gates, whose files are test code in their own right.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TestGated {
    /// Each gated item's lines, 1-based and both included: from the attribute to the item's end.
    /// A `#![cfg(test)]` at the top gates the whole file.
    pub lines: Vec<(usize, usize)>,
    /// Each gated `mod name;`: its name, and the file a `#[path = "…"]` on it names.
    pub module_files: Vec<(String, Option<String>)>,
}

impl TestGated {
    /// Whether `line` (1-based) is inside a gated item.
    pub fn covers(&self, line: usize) -> bool {
        self.lines
            .iter()
            .any(|(first, last)| (*first..=*last).contains(&line))
    }
}

/// What each `#[cfg(test)]` in `source` gates: the one item directly under it, which is a whole
/// module when that item is a `mod`. A `#[cfg(test)] use …` gates that `use` and nothing else, so
/// it never makes the code after it test code.
pub fn test_gated(source: &str) -> TestGated {
    let code = blank_literals(source);
    let newlines: Vec<usize> = code
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
        .collect();
    let mut gated = TestGated::default();
    if find_text(&code, 0, b"#![cfg(test)]").is_some() {
        gated.lines.push((1, newlines.len() + 1));
        return gated;
    }
    let attribute = b"#[cfg(test)]";
    let mut at = 0;
    while let Some(offset) = find_text(&code, at, attribute) {
        at = offset + attribute.len();
        let (item, path) = skip_attributes(source, &code, at);
        let end = item_end(&code, item);
        gated
            .lines
            .push((line_of(&newlines, offset), line_of(&newlines, end)));
        if code.get(end) == Some(&b';')
            && let Some(name) = module_name(&code[item..end])
        {
            gated.module_files.push((name, path));
        }
    }
    gated
}

/// The next occurrence of `text` at or after `from`.
fn find_text(code: &[u8], from: usize, text: &[u8]) -> Option<usize> {
    code.get(from..)?
        .windows(text.len())
        .position(|window| window == text)
        .map(|offset| from + offset)
}

/// Past any further attributes from `from`: where the item starts, and the file a `#[path = "…"]`
/// among them names (read from `source`, since `code` has its strings blanked).
fn skip_attributes(source: &str, code: &[u8], from: usize) -> (usize, Option<String>) {
    let mut index = skip_space(code, from);
    let mut path = None;
    while code.get(index) == Some(&b'#') && code.get(index + 1) == Some(&b'[') {
        let Some(close) = matching(code, index + 1, b'[', b']') else {
            break;
        };
        let text = source.get(index..=close).unwrap_or_default();
        if let Some(rest) = text.strip_prefix("#[path") {
            path = rest.split('"').nth(1).map(str::to_owned);
        }
        index = skip_space(code, close + 1);
    }
    (index, path)
}

/// Where the item starting at `from` ends: its closing brace, or the `;` that ends it first.
fn item_end(code: &[u8], from: usize) -> usize {
    let mut depth = 0i32;
    for (index, byte) in code.iter().enumerate().skip(from) {
        match byte {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'{' if depth == 0 => return matching(code, index, b'{', b'}').unwrap_or(code.len()),
            b';' if depth == 0 => return index,
            _ => {}
        }
    }
    code.len().saturating_sub(1)
}

/// `name` for an item that reads `mod name` or `pub(…) mod name`.
fn module_name(item: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(item);
    let mut words = text.split_whitespace();
    let mut word = words.next()?;
    if word.starts_with("pub") {
        word = words.next()?;
    }
    (word == "mod").then(|| words.next().map(str::to_owned))?
}

/// Every `impl`, `trait` and inline `mod` block: where it starts and ends, and its label.
fn scopes(code: &[u8]) -> Vec<(usize, usize, String)> {
    let mut scopes = Vec::new();
    for keyword in [&b"impl"[..], b"trait", b"mod"] {
        let mut at = 0;
        while let Some(offset) = find_keyword(code, at, keyword) {
            at = offset + keyword.len();
            if keyword == b"impl" && !at_item_start(code, offset) {
                continue; // `-> impl Trait`, `x: impl Fn()`: a type, not a block
            }
            let Some(open) = block_open(code, at) else {
                continue;
            };
            let Some(close) = matching(code, open, b'{', b'}') else {
                continue;
            };
            let header = String::from_utf8_lossy(&code[at..open]).into_owned();
            let label = if keyword == b"impl" {
                impl_label(&header)
            } else {
                header
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            };
            scopes.push((open, close, label));
        }
    }
    scopes
}

/// Whether the keyword at `offset` starts an item: nothing but whitespace, `unsafe` or an
/// attribute's `]` since the previous item or block boundary.
fn at_item_start(code: &[u8], offset: usize) -> bool {
    let before = code[..offset].trim_ascii_end();
    before.is_empty()
        || matches!(before.last(), Some(b'}' | b';' | b'{' | b']'))
        || before.ends_with(b"unsafe")
}

/// The `{` opening the block whose header starts at `from`, or `None` if a `;` ends it first.
fn block_open(code: &[u8], from: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (index, byte) in code.iter().enumerate().skip(from) {
        match byte {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'{' if depth == 0 => return Some(index),
            b';' if depth == 0 => return None,
            _ => {}
        }
    }
    None
}

/// `Type` for an inherent impl and `<Type as Trait>` for a trait impl, without generic arguments,
/// lifetimes or references.
fn impl_label(header: &str) -> String {
    let mut text = header.trim();
    if text.starts_with('<')
        && let Some(close) = closing_angle(text.as_bytes(), 0)
    {
        text = &text[close + 1..];
    }
    if let Some(at) = find_word(text, "where") {
        text = &text[..at];
    }
    match find_word(text, "for") {
        Some(at) => format!(
            "<{} as {}>",
            simple_type(&text[at + 3..]),
            simple_type(&text[..at])
        ),
        None => simple_type(text),
    }
}

/// `text` without generic arguments, lifetimes, `&`, `mut` or `dyn`, and with single spaces.
fn simple_type(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.replace('&', " ")
        .split_whitespace()
        .filter(|word| !word.starts_with('\'') && !matches!(*word, "mut" | "dyn"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where `word` stands as a whole word in `text`.
fn find_word(text: &str, word: &str) -> Option<usize> {
    find_keyword(text.as_bytes(), 0, word.as_bytes())
}

/// The next `keyword` at or after `from` that stands as a whole word.
fn find_keyword(code: &[u8], from: usize, keyword: &[u8]) -> Option<usize> {
    let mut index = from;
    while index + keyword.len() <= code.len() {
        if &code[index..index + keyword.len()] == keyword
            && (index == 0 || !is_ident(code[index - 1]))
            && code
                .get(index + keyword.len())
                .is_none_or(|b| !is_ident(*b))
        {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// The function whose `fn` keyword is at `offset` and the end of its body, or `None` for a pointer
/// type (`fn(u8)`), a macro pattern (`fn $name`) or a declaration without a body.
fn function_at(code: &[u8], newlines: &[usize], offset: usize) -> Option<(FunctionSpan, usize)> {
    let mut index = skip_space(code, offset + 2);
    let name_start = index;
    while index < code.len() && is_ident(code[index]) {
        index += 1;
    }
    if index == name_start {
        return None;
    }
    let name = String::from_utf8_lossy(&code[name_start..index]).into_owned();
    index = skip_space(code, index);
    // Generics sit between the name and the parameters, and may hold `Fn(A, B) -> C` bounds.
    if code.get(index) == Some(&b'<') {
        index = closing_angle(code, index)? + 1;
    }
    let open = skip_space(code, index);
    if code.get(open) != Some(&b'(') {
        return None;
    }
    let close = matching(code, open, b'(', b')')?;
    let params = count_params(&code[open + 1..close]);
    let body = close
        + code[close..]
            .iter()
            .position(|b| matches!(b, b'{' | b';'))?;
    if code[body] == b';' {
        return None;
    }
    let end = matching(code, body, b'{', b'}')?;
    let line = line_of(newlines, offset);
    let span = FunctionSpan {
        qualified: name.clone(),
        name,
        line,
        lines: line_of(newlines, end) - line + 1,
        params,
    };
    Some((span, end))
}

fn skip_space(code: &[u8], from: usize) -> usize {
    from + code[from..]
        .iter()
        .take_while(|b| b.is_ascii_whitespace())
        .count()
}

/// The index of the bracket that closes the one at `open`.
fn matching(code: &[u8], open: usize, left: u8, right: u8) -> Option<usize> {
    let mut depth = 0usize;
    for (index, byte) in code.iter().enumerate().skip(open) {
        if *byte == left {
            depth += 1;
        } else if *byte == right {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

/// The `>` that closes the `<` at `open`, skipping the `>` of every `->`.
fn closing_angle(code: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, byte) in code.iter().enumerate().skip(open) {
        match byte {
            b'<' => depth += 1,
            b'>' if code[index - 1] != b'-' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            b'{' | b';' => return None,
            _ => {}
        }
    }
    None
}

/// Top-level commas in a parameter list, ignoring a trailing one. Angle brackets count as depth,
/// except the `>` of an `->` in a function-pointer type.
fn count_params(list: &[u8]) -> usize {
    if list.iter().all(u8::is_ascii_whitespace) {
        return 0;
    }
    let mut depth = 0i32;
    let mut commas = 0;
    let mut last_comma = None;
    for (index, byte) in list.iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b'>' if index > 0 && list[index - 1] == b'-' => {}
            b')' | b']' | b'}' | b'>' => depth -= 1,
            b',' if depth == 0 => {
                commas += 1;
                last_comma = Some(index);
            }
            _ => {}
        }
    }
    let trailing = last_comma.is_some_and(|at| list[at + 1..].iter().all(u8::is_ascii_whitespace));
    commas + 1 - usize::from(trailing)
}

/// The 1-based line of `offset`, from the offsets of every newline in the file.
fn line_of(newlines: &[usize], offset: usize) -> usize {
    newlines.partition_point(|newline| *newline < offset) + 1
}

#[cfg(test)]
mod tests {
    use super::{FunctionSpan, TestGated, functions, test_gated};

    fn spans(source: &str) -> Vec<(String, usize, usize, usize)> {
        functions(source)
            .into_iter()
            .map(
                |FunctionSpan {
                     name,
                     line,
                     lines,
                     params,
                     ..
                 }| (name, line, lines, params),
            )
            .collect()
    }

    #[test]
    fn a_function_spans_its_fn_line_to_its_closing_brace() {
        let source = "fn one(a: u8, b: u8) -> u8 {\n    a + b\n}\n\nfn two() {}\n";
        assert_eq!(
            spans(source),
            [("one".into(), 1, 3, 2), ("two".into(), 5, 1, 0)]
        );
    }

    #[test]
    fn braces_in_strings_comments_and_characters_are_not_counted() {
        let source = "fn f() {\n    let s = \"}{\"; // }\n    /* } /* nested } */ */\n    let c = '}';\n    let r = r#\"}\"#;\n    let l: &'static str = s;\n}\nfn g() {}\n";
        assert_eq!(
            spans(source),
            [("f".into(), 1, 7, 0), ("g".into(), 8, 1, 0)]
        );
    }

    #[test]
    fn generics_function_pointers_and_trailing_commas_count_as_one_parameter_each() {
        let source = "fn f<T: Fn(u8, u8) -> u8>(\n    map: HashMap<String, u8>,\n    call: fn(u8, u16) -> u8,\n    &self,\n) {}\n";
        assert_eq!(spans(source), [("f".into(), 1, 5, 3)]);
    }

    #[test]
    fn declarations_pointer_types_and_macro_patterns_are_not_functions() {
        let source = "trait T { fn decl(&self); }\ntype P = fn(u8) -> u8;\nmacro_rules! m { ($n:ident) => { fn $n() {} } }\n";
        assert!(spans(source).is_empty());
    }

    #[test]
    fn a_function_is_named_inside_its_impl_trait_module_and_parent() {
        let source = "impl<S: Store> Edge<S> where S: Send {\n    fn open(&self) -> impl Iterator<Item = u8> { inner() }\n}\n\
            impl<'a, T> fmt::Display for &'a Bill<T> {\n    fn fmt(&self) {}\n}\n\
            trait Port { fn ready(&self) -> bool { true } }\n\
            mod tests {\n    fn helper() {\n        fn deep() {}\n    }\n}\nmod other;\nfn free() {}\n";
        let names: Vec<String> = functions(source).into_iter().map(|f| f.qualified).collect();
        assert_eq!(
            names,
            [
                "Edge::open",
                "<Bill as fmt::Display>::fmt",
                "Port::ready",
                "tests::helper",
                "tests::helper::deep",
                "free",
            ]
        );
    }

    #[test]
    fn a_gated_use_gates_only_itself_not_the_code_below_it() {
        // The loophole this rule closes: a `#[cfg(test)] use` near the top once made every
        // function below it test code, and so exempt from the size limits.
        let source = "#[cfg(test)]\nuse crate::helper;\n\nfn production() {\n    work();\n}\n";
        let gated = test_gated(source);
        assert_eq!(gated.lines, [(1, 2)]);
        assert!(
            !gated.covers(4),
            "the function below the gated use is production code"
        );
        assert!(gated.module_files.is_empty());
    }

    #[test]
    fn a_gated_inline_module_is_test_code_to_its_closing_brace() {
        let source = "fn production() {}\n\n#[cfg(test)]\nmod tests {\n    fn helper() {}\n\n    #[test]\n    fn case() {}\n}\n\nfn after() {}\n";
        let gated = test_gated(source);
        assert_eq!(gated.lines, [(3, 9)]);
        assert!(!gated.covers(1));
        assert!(gated.covers(5) && gated.covers(8));
        assert!(
            !gated.covers(11),
            "a function after the module is production code"
        );
    }

    #[test]
    fn a_gated_function_is_test_code_and_the_next_one_is_not() {
        let source = "#[cfg(test)]\n#[allow(dead_code)]\nfn only_for_tests() {\n    let s = \"}\";\n}\nfn production() {}\n";
        let gated = test_gated(source);
        assert_eq!(gated.lines, [(1, 5)]);
        assert!(!gated.covers(6));
    }

    #[test]
    fn a_gated_module_file_is_named_with_its_path_attribute() {
        let source = "#[cfg(test)]\nmod tests;\n\n#[cfg(test)]\n#[path = \"cloud/extra.rs\"]\npub(crate) mod extra;\nfn production() {}\n";
        let gated = test_gated(source);
        assert_eq!(
            gated.module_files,
            [
                ("tests".to_owned(), None),
                ("extra".to_owned(), Some("cloud/extra.rs".to_owned())),
            ]
        );
        assert!(!gated.covers(7));
    }

    #[test]
    fn an_inner_cfg_test_gates_the_whole_file() {
        let source = "#![cfg(test)]\n\nfn helper() {}\n";
        assert!(test_gated(source).covers(3));
    }

    #[test]
    fn a_cfg_test_inside_a_string_or_comment_gates_nothing() {
        let source = "// #[cfg(test)]\nfn production() { let s = \"#[cfg(test)]\"; }\n";
        assert_eq!(test_gated(source), TestGated::default());
    }

    #[test]
    fn a_nested_function_is_counted_on_its_own_and_inside_its_parent() {
        let source = "fn outer() {\n    fn inner(x: u8) {\n    }\n}\n";
        assert_eq!(
            spans(source),
            [("outer".into(), 1, 4, 0), ("inner".into(), 2, 2, 1)]
        );
    }
}
