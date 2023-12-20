use std::fmt::Display;

use anyhow::Result;
use utf8_chars::BufReadCharsExt;

#[derive(Debug, PartialEq)]
pub(crate) enum TokenEndReason {
    /// End of input was reached.
    EndOfInput,
    /// An unescaped newline char was reached.
    UnescapedNewLine,
    /// Specified terminating char.
    SpecifiedTerminatingChar,
    /// A non-newline blank char was reached.
    NonNewLineBlank,
    /// A non-newline token-delimiting char was encountered.
    Other,
}

#[derive(Clone, Debug)]
pub struct SourcePosition {
    pub line: i32,
    pub column: i32,
}

impl Display for SourcePosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("line {} col {}", self.line, self.column))
    }
}

#[derive(Clone, Debug)]
pub struct TokenLocation {
    pub start: SourcePosition,
    pub end: SourcePosition,
}

#[derive(Clone, Debug)]
pub enum Token {
    Operator(String, TokenLocation),
    Word((String, ParsedWord), TokenLocation),
}

impl Token {
    pub fn to_str(&self) -> &str {
        match self {
            Token::Operator(s, _) => s,
            Token::Word((s, _), _) => s,
        }
    }

    pub fn location(&self) -> &TokenLocation {
        match self {
            Token::Operator(_, l) => l,
            Token::Word(_, l) => l,
        }
    }
}

pub type ParsedWord = Vec<WordSubtoken>;

#[derive(Clone, Debug)]
pub enum WordSubtoken {
    Text(String),
    SingleQuotedText(String),
    DoubleQuotedSequence(String, Vec<WordSubtoken>),
    CommandSubstitution(String, Vec<Token>),
    EscapeSequence(String),
}

impl WordSubtoken {
    pub fn to_str(&self) -> &str {
        match self {
            WordSubtoken::Text(s) => s,
            WordSubtoken::CommandSubstitution(s, _) => s,
            WordSubtoken::SingleQuotedText(s) => s,
            WordSubtoken::DoubleQuotedSequence(s, _) => s,
            WordSubtoken::EscapeSequence(s) => s,
        }
    }
}

#[derive(Debug)]
pub(crate) struct TokenizeResult {
    pub reason: TokenEndReason,
    pub token: Option<Token>,
}

#[derive(Debug)]
pub(crate) struct Tokens<'a> {
    pub tokens: &'a [Token],
}

#[derive(Clone, Debug)]
enum QuoteMode {
    None,
    Single(SourcePosition),
    Double(SourcePosition),
}

#[derive(Clone, Debug, PartialEq)]
enum HereState {
    None,
    NextTokenIsHereTag { remove_tabs: bool },
    CurrentTokenIsHereTag { remove_tabs: bool },
    NextLineIsHereDoc,
    InHereDocs,
}

#[derive(Clone, Debug)]
struct HereTag {
    tag: String,
    remove_tabs: bool,
}

#[derive(Clone, Debug)]
struct CrossTokenParseState {
    cursor: SourcePosition,
    here_state: HereState,
    current_here_tags: Vec<HereTag>,
}

pub(crate) struct Tokenizer<'a, R: ?Sized + std::io::BufRead> {
    char_reader: std::iter::Peekable<utf8_chars::Chars<'a, R>>,
    cross_state: CrossTokenParseState,
}

#[derive(Clone, Debug)]
struct TokenParseState {
    pub start_position: SourcePosition,
    pub completed_subtokens: Vec<WordSubtoken>,
    pub subtoken_stack: Vec<WordSubtoken>,
    pub token_so_far: String,
    pub token_is_operator: bool,
    pub in_escape: bool,
    pub quote_mode: QuoteMode,
}

impl TokenParseState {
    pub fn new(start_position: &SourcePosition) -> Self {
        TokenParseState {
            start_position: start_position.clone(),
            completed_subtokens: vec![],
            subtoken_stack: vec![],
            token_so_far: String::new(),
            token_is_operator: false,
            in_escape: false,
            quote_mode: QuoteMode::None,
        }
    }

    pub fn pop(&mut self, end_position: &SourcePosition) -> Result<Token> {
        while !self.subtoken_stack.is_empty() {
            self.delimit_current_subtoken();
        }

        let token_location = TokenLocation {
            start: self.start_position.clone(),
            end: end_position.clone(),
        };

        let token = if self.token_is_operator {
            Token::Operator(std::mem::take(&mut self.token_so_far), token_location)
        } else {
            Token::Word(
                (
                    std::mem::take(&mut self.token_so_far),
                    std::mem::take(&mut self.completed_subtokens),
                ),
                token_location,
            )
        };

        Ok(token)
    }

    pub fn started_token(&self) -> bool {
        !self.token_so_far.is_empty()
    }

    pub fn should_start_text_subtoken(&self) -> bool {
        matches!(
            self.subtoken_stack.last(),
            Some(WordSubtoken::DoubleQuotedSequence(_, _)) | None
        )
    }

    pub fn append_char(&mut self, c: char) {
        self.token_so_far.push(c);

        if self.subtoken_stack.is_empty() {
            panic!("appending char '{c}' without current subtoken")
        }

        for subtoken in self.subtoken_stack.iter_mut() {
            match subtoken {
                WordSubtoken::Text(text) => text.push(c),
                WordSubtoken::SingleQuotedText(text) => text.push(c),
                WordSubtoken::DoubleQuotedSequence(text, _) => text.push(c),
                WordSubtoken::EscapeSequence(text) => text.push(c),
                WordSubtoken::CommandSubstitution(text, _) => text.push(c),
            }
        }
    }

    pub fn append_str(&mut self, s: &str) {
        self.token_so_far.push_str(s);

        if self.subtoken_stack.is_empty() {
            panic!("appending string '{s}' without current subtoken")
        }

        for subtoken in self.subtoken_stack.iter_mut() {
            match subtoken {
                WordSubtoken::Text(text) => text.push_str(s),
                WordSubtoken::SingleQuotedText(text) => text.push_str(s),
                WordSubtoken::DoubleQuotedSequence(text, _) => text.push_str(s),
                WordSubtoken::EscapeSequence(text) => text.push_str(s),
                WordSubtoken::CommandSubstitution(text, _) => text.push_str(s),
            }
        }
    }

    pub fn unquoted(&self) -> bool {
        !self.in_escape && matches!(self.quote_mode, QuoteMode::None)
    }

    pub fn delimit_current_subtoken(&mut self) {
        if let Some(current_subtoken) = self.subtoken_stack.pop() {
            if let Some(WordSubtoken::DoubleQuotedSequence(_, subtokens)) =
                self.subtoken_stack.last_mut()
            {
                subtokens.push(current_subtoken);
            } else {
                self.completed_subtokens.push(current_subtoken)
            }
        }
    }

    pub fn start_subtoken<F>(&mut self, f: F)
    where
        F: Fn() -> WordSubtoken,
    {
        // First check to see what subtoken is on top of the stack (if any).
        match self.subtoken_stack.last() {
            Some(WordSubtoken::DoubleQuotedSequence(_, _))
            | Some(WordSubtoken::CommandSubstitution(_, _)) => (),
            Some(_) => self.delimit_current_subtoken(),
            _ => (),
        }

        self.subtoken_stack.push(f());
    }

    pub fn current_token(&self) -> &str {
        &self.token_so_far
    }

    pub fn is_specific_operator(&self, operator: &str) -> bool {
        self.token_is_operator && self.current_token() == operator
    }

    pub fn is_operator(&self) -> bool {
        self.token_is_operator
    }

    fn is_newline(&self) -> bool {
        self.token_so_far == "\n"
    }

    fn replace_with_here_doc(&mut self, s: String) {
        if let Some(WordSubtoken::Text(text)) = self.subtoken_stack.last_mut() {
            text.clear();
            text.push_str(s.as_str());
        }
        self.token_so_far = s;
    }

    pub fn delimit_current_token(
        &mut self,
        reason: TokenEndReason,
        cross_token_state: &mut CrossTokenParseState,
    ) -> Result<TokenizeResult> {
        if !self.started_token() {
            return Ok(TokenizeResult {
                reason,
                token: None,
            });
        }

        // TODO: Make sure the here-tag meets criteria (and isn't a newline).
        match cross_token_state.here_state {
            HereState::NextTokenIsHereTag { remove_tabs } => {
                cross_token_state.here_state = HereState::CurrentTokenIsHereTag { remove_tabs };
            }
            HereState::CurrentTokenIsHereTag { remove_tabs } => {
                if self.is_newline() {
                    return Err(anyhow::anyhow!(
                        "Missing here tag '{}'",
                        self.current_token()
                    ));
                }

                cross_token_state.here_state = HereState::NextLineIsHereDoc;

                if self.current_token().contains('\"')
                    || self.current_token().contains('\'')
                    || self.current_token().contains('\\')
                {
                    todo!("UNIMPLEMENTED: quoted or escaped here tag");
                }

                // Include the \n in the here tag so it's easier to check against.
                cross_token_state.current_here_tags.push(HereTag {
                    tag: std::format!("\n{}\n", self.current_token()),
                    remove_tabs,
                });
            }
            HereState::NextLineIsHereDoc => {
                if self.is_newline() {
                    cross_token_state.here_state = HereState::InHereDocs;
                }
            }
            _ => (),
        }

        let token = Some(self.pop(&cross_token_state.cursor)?);
        Ok(TokenizeResult { reason, token })
    }
}

pub fn tokenize_str(input: &str) -> Result<Vec<Token>> {
    let mut reader = std::io::BufReader::new(input.as_bytes());
    let mut tokenizer = crate::tokenizer::Tokenizer::new(&mut reader);

    let mut tokens = vec![];
    while let Some(token) = tokenizer.next_token()?.token {
        tokens.push(token);
    }

    Ok(tokens)
}

impl<'a, R: ?Sized + std::io::BufRead> Tokenizer<'a, R> {
    pub fn new(reader: &'a mut R) -> Tokenizer<'a, R> {
        Tokenizer {
            char_reader: reader.chars().peekable(),
            cross_state: CrossTokenParseState {
                cursor: SourcePosition { line: 1, column: 1 },
                here_state: HereState::None,
                current_here_tags: vec![],
            },
        }
    }

    pub fn current_location(&self) -> Option<SourcePosition> {
        Some(self.cross_state.cursor.clone())
    }

    fn next_char(&mut self) -> Result<Option<char>> {
        let c = self.char_reader.next().transpose()?;

        if let Some(ch) = c {
            if ch == '\n' {
                self.cross_state.cursor.line += 1;
                self.cross_state.cursor.column = 1;
            } else {
                self.cross_state.cursor.column += 1;
            }
        }

        Ok(c)
    }

    fn consume_char(&mut self) -> Result<()> {
        let _ = self.next_char()?;
        Ok(())
    }

    fn peek_char(&mut self) -> Result<Option<char>> {
        match self.char_reader.peek() {
            Some(result) => match result {
                Ok(c) => Ok(Some(*c)),
                Err(_) => Err(anyhow::anyhow!("failed to decode UTF-8 characters")),
            },
            None => Ok(None),
        }
    }

    pub fn next_token(&mut self) -> Result<TokenizeResult> {
        self.next_token_until(None)
    }

    fn next_token_until(&mut self, terminating_char: Option<char>) -> Result<TokenizeResult> {
        let mut state = TokenParseState::new(&self.cross_state.cursor);

        loop {
            let next = self.peek_char()?;
            let c = next.unwrap_or('\0');

            if next.is_none() {
                // Verify we're out of all quotes.
                if state.in_escape {
                    return Err(anyhow::anyhow!("unterminated escape sequence"));
                }
                match state.quote_mode {
                    QuoteMode::None => (),
                    QuoteMode::Single(pos) => {
                        return Err(anyhow::anyhow!("unterminated single quote at {}", pos))
                    }
                    QuoteMode::Double(pos) => {
                        return Err(anyhow::anyhow!("unterminated double quote at {}", pos))
                    }
                }

                // Verify we're not in a here document.
                if self.cross_state.here_state != HereState::None {
                    return Err(anyhow::anyhow!("unterminated here document sequence"));
                }

                return state
                    .delimit_current_token(TokenEndReason::EndOfInput, &mut self.cross_state);
            //
            // Look for the specially specified terminating char.
            //
            } else if state.unquoted() && terminating_char == Some(c) {
                return state.delimit_current_token(
                    TokenEndReason::SpecifiedTerminatingChar,
                    &mut self.cross_state,
                );
            //
            // Handle being in a here document.
            //
            } else if self.cross_state.here_state == HereState::InHereDocs {
                //
                // For now, just include the character in the current token. We also check
                // if there are leading tabs to be removed.
                //
                self.consume_char()?;
                if !self.cross_state.current_here_tags.is_empty()
                    && self.cross_state.current_here_tags[0].remove_tabs
                    && (!state.started_token() || state.current_token().ends_with('\n'))
                    && c == '\t'
                {
                    // Nothing to do.
                } else {
                    if state.should_start_text_subtoken() {
                        state.start_subtoken(|| WordSubtoken::Text(String::new()));
                    }
                    state.append_char(c);
                }
            } else if state.is_operator() {
                let mut hypothetical_token = state.current_token().to_owned();
                hypothetical_token.push(c);

                if state.unquoted() && is_operator(hypothetical_token.as_ref()) {
                    self.consume_char()?;
                    state.append_char(c);
                } else {
                    assert!(state.started_token());

                    //
                    // N.B. If the completed operator indicates a here-document, then keep
                    // track that the *next* token should be the here-tag.
                    //
                    if state.is_specific_operator("<<") {
                        self.cross_state.here_state =
                            HereState::NextTokenIsHereTag { remove_tabs: false };
                    } else if state.is_specific_operator("<<-") {
                        self.cross_state.here_state =
                            HereState::NextTokenIsHereTag { remove_tabs: true };
                    }

                    return state
                        .delimit_current_token(TokenEndReason::Other, &mut self.cross_state);
                }
            } else if does_char_newly_affect_quoting(&state, c) {
                if c == '\\' {
                    // Consume the backslash ourselves so we can peek past it.
                    self.consume_char()?;

                    if self.peek_char()? == Some('\n') {
                        // Make sure the newline char gets consumed too.
                        self.consume_char()?;

                        // Make sure to include neither the backslash nor the newline character.
                    } else {
                        state.start_subtoken(|| WordSubtoken::EscapeSequence(String::new()));
                        state.in_escape = true;
                        state.append_char(c);
                    }
                } else if c == '\'' {
                    state.start_subtoken(|| WordSubtoken::SingleQuotedText(String::new()));
                    state.quote_mode = QuoteMode::Single(self.cross_state.cursor.clone());
                    self.consume_char()?;
                    state.append_char(c);
                } else if c == '\"' {
                    state.start_subtoken(|| {
                        WordSubtoken::DoubleQuotedSequence(String::new(), vec![])
                    });
                    state.quote_mode = QuoteMode::Double(self.cross_state.cursor.clone());
                    self.consume_char()?;
                    state.append_char(c);
                }
            }
            //
            // Handle end of single-quote or double-quote.
            //
            else if !state.in_escape
                && matches!(state.quote_mode, QuoteMode::Single(_))
                && c == '\''
            {
                state.quote_mode = QuoteMode::None;
                self.consume_char()?;
                state.append_char(c);
                state.delimit_current_subtoken();
            } else if !state.in_escape
                && matches!(state.quote_mode, QuoteMode::Double(_))
                && c == '\"'
            {
                if !matches!(
                    state.subtoken_stack.last(),
                    Some(WordSubtoken::DoubleQuotedSequence(_, _))
                ) {
                    state.delimit_current_subtoken();
                }

                state.quote_mode = QuoteMode::None;
                self.consume_char()?;
                state.append_char(c);
                state.delimit_current_subtoken();
            }
            //
            // Handle end of escape sequence.
            // TODO: Handle double-quote specific escape sequences.
            //
            else if state.in_escape {
                state.in_escape = false;
                self.consume_char()?;
                state.append_char(c);
                state.delimit_current_subtoken();
            } else if (state.unquoted()
                || (matches!(state.quote_mode, QuoteMode::Double(_)) && !state.in_escape))
                && (c == '$' || c == '`')
            {
                // TODO: handle quoted $ or ` in a double quote
                if c == '$' {
                    // Consume the '$' so we can peek beyond.
                    self.consume_char()?;

                    // Now peek beyond to see what we have.
                    let char_after_dollar_sign = self.peek_char()?;
                    if let Some(cads) = char_after_dollar_sign {
                        match cads {
                            '(' => {
                                state.start_subtoken(|| {
                                    WordSubtoken::CommandSubstitution(String::new(), vec![])
                                });

                                // Add the '$' we already consumed to the token.
                                state.append_char('$');

                                // Consume the '(' and add it to the token.
                                state.append_char(self.next_char()?.unwrap());

                                let mut tokens = vec![];

                                loop {
                                    let cur_token = self.next_token_until(Some(')'))?;
                                    if let Some(cur_token_value) = cur_token.token {
                                        if !tokens.is_empty() {
                                            state.append_char(' ');
                                        }

                                        state.append_str(cur_token_value.to_str());
                                        tokens.push(cur_token_value);
                                    }

                                    if cur_token.reason == TokenEndReason::SpecifiedTerminatingChar
                                    {
                                        // We hit the ')' we were looking for.
                                        break;
                                    }
                                }

                                state.append_char(self.next_char()?.unwrap());

                                if let Some(WordSubtoken::CommandSubstitution(_, existing_tokens)) =
                                    state.subtoken_stack.last_mut()
                                {
                                    existing_tokens.append(&mut tokens);
                                } else {
                                    panic!("expected command substitution subtoken");
                                }

                                state.delimit_current_subtoken();
                            }

                            '{' => {
                                // Add the '$' we already consumed to the token.
                                if state.should_start_text_subtoken() {
                                    state.start_subtoken(|| WordSubtoken::Text(String::new()));
                                }
                                state.append_char('$');

                                // Consume the '{' and add it to the token.
                                state.append_char(self.next_char()?.unwrap());

                                loop {
                                    let cur_token = self.next_token_until(Some('}'))?;
                                    if let Some(cur_token_value) = cur_token.token {
                                        state.append_str(cur_token_value.to_str())
                                    }

                                    if cur_token.reason == TokenEndReason::NonNewLineBlank {
                                        state.append_char(' ');
                                    }

                                    if cur_token.reason == TokenEndReason::SpecifiedTerminatingChar
                                    {
                                        // We hit the end brace we were looking for but did not
                                        // yet consume it. Do so now.
                                        state.append_char(self.next_char()?.unwrap());
                                        break;
                                    }
                                }
                            }
                            _ => {
                                // Add the '$' we already consumed to the token.
                                if state.should_start_text_subtoken() {
                                    state.start_subtoken(|| WordSubtoken::Text(String::new()));
                                }
                                state.append_char('$');
                            }
                        }
                    }
                } else {
                    // We look for the terminating backquote. First disable normal consumption and consume
                    // the starting backquote.
                    let backquote_loc = self.cross_state.cursor.clone();
                    self.consume_char()?;

                    state.start_subtoken(|| {
                        WordSubtoken::CommandSubstitution(String::new(), vec![])
                    });

                    // Add the opening backquote to the token.
                    state.append_char(c);

                    // Now continue until we see an unescaped backquote.
                    let mut escaping_enabled = false;
                    let mut done = false;
                    while !done {
                        // Read (and consume) the next char.
                        let next_char_in_backquote = self.next_char()?;
                        if let Some(cib) = next_char_in_backquote {
                            // Include it in the token no matter what.
                            state.append_char(cib);

                            // Watch out for escaping.
                            if !escaping_enabled && cib == '\\' {
                                escaping_enabled = true;
                            } else {
                                // Look for an unescaped backquote to terminate.
                                if !escaping_enabled && cib == '`' {
                                    done = true;
                                }
                                escaping_enabled = false;
                            }
                        } else {
                            return Err(anyhow::anyhow!(
                                "Unterminated backquote near {}",
                                backquote_loc
                            ));
                        }
                    }

                    state.delimit_current_subtoken();
                }
            } else if state.unquoted() && can_start_operator(c) {
                if state.started_token() {
                    return state
                        .delimit_current_token(TokenEndReason::Other, &mut self.cross_state);
                } else {
                    if state.should_start_text_subtoken() {
                        state.start_subtoken(|| WordSubtoken::Text(String::new()));
                    }
                    state.token_is_operator = true;
                    self.consume_char()?;
                    state.append_char(c);
                }
            } else if state.unquoted() && is_blank(c) {
                self.consume_char()?;

                if state.started_token() {
                    return state.delimit_current_token(
                        TokenEndReason::NonNewLineBlank,
                        &mut self.cross_state,
                    );
                }
            }
            //
            // N.B. We need to remember if we were recursively called, say in a command
            // substitution; in that case we won't think a token was started but... we'd
            // be wrong.
            //
            else if !state.token_is_operator
                && (state.started_token() || terminating_char.is_some())
            {
                if state.should_start_text_subtoken() {
                    state.start_subtoken(|| WordSubtoken::Text(String::new()));
                }
                self.consume_char()?;
                state.append_char(c);
            } else if c == '#' {
                // Consume the '#'.
                self.consume_char()?;

                let mut done = false;
                while !done {
                    done = match self.peek_char()? {
                        Some('\n') => true,
                        None => true,
                        _ => {
                            // Consume the peeked char; it's part of the comment.
                            self.consume_char()?;
                            false
                        }
                    };
                }

                // Re-start loop as if the comment never happened.
                continue;
            } else if state.started_token() {
                return state.delimit_current_token(TokenEndReason::Other, &mut self.cross_state);
            } else {
                if state.should_start_text_subtoken() {
                    state.start_subtoken(|| WordSubtoken::Text(String::new()));
                }
                self.consume_char()?;
                state.append_char(c);
            }

            //
            // Now update state.
            //

            // Check for the end of a here-document.
            if self.cross_state.here_state == HereState::InHereDocs
                && !self.cross_state.current_here_tags.is_empty()
            {
                let without_suffix = state
                    .current_token()
                    .strip_suffix(self.cross_state.current_here_tags[0].tag.as_str())
                    .map(|s| s.to_owned());

                if let Some(without_suffix) = without_suffix {
                    // We hit the end of the here document.
                    self.cross_state.current_here_tags.remove(0);
                    if self.cross_state.current_here_tags.is_empty() {
                        self.cross_state.here_state = HereState::None;
                    }

                    state.replace_with_here_doc(without_suffix);
                    state.append_char('\n');
                }
            }
        }
    }
}

impl<'a, R: ?Sized + std::io::BufRead> Iterator for Tokenizer<'a, R> {
    type Item = Result<TokenizeResult>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_token() {
            #[allow(clippy::manual_map)]
            Ok(result) => match result.token {
                Some(_) => Some(Ok(result)),
                None => None,
            },
            Err(e) => Some(Err(e)),
        }
    }
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn can_start_operator(c: char) -> bool {
    matches!(c, '&' | '(' | ')' | ';' | '\n' | '|' | '<' | '>')
}

fn does_char_newly_affect_quoting(state: &TokenParseState, c: char) -> bool {
    // If we're currently escaped, then nothing affects quoting.
    if state.in_escape {
        return false;
    }

    match state.quote_mode {
        // When we're in a double quote, only a subset of escape sequences are recognized.
        QuoteMode::Double(_) => {
            if c == '\\' {
                // TODO: handle backslash in double quote
                true
            } else {
                false
            }
        }
        // When we're in a single quote, nothing affects quoting.
        QuoteMode::Single(_) => false,
        // When we're not already in a quote, then we can straightforwardly look for a
        // quote mark or backslash.
        QuoteMode::None => is_quoting_char(c),
    }
}

fn is_quoting_char(c: char) -> bool {
    matches!(c, '\\' | '\'' | '\"')
}

fn is_operator(s: &str) -> bool {
    matches!(
        s,
        "&" | "&&"
            | "("
            | ")"
            | ";"
            | ";;"
            | "\n"
            | "|"
            | "||"
            | "<"
            | ">"
            | ">|"
            | "<<"
            | ">>"
            | "<&"
            | ">&"
            | "<<-"
            | "<>"
    )
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;

    #[test]
    fn tokenize_empty() -> Result<()> {
        let tokens = tokenize_str("")?;
        assert_eq!(tokens.len(), 0);
        Ok(())
    }

    #[test]
    fn tokenize_line_continuation() -> Result<()> {
        let tokens = tokenize_str(
            r"a\
bc",
        )?;
        assert_matches!(
            &tokens[..],
            [t1 @ Token::Word(_, _)] if t1.to_str() == "abc"
        );
        Ok(())
    }

    #[test]
    fn tokenize_operators() -> Result<()> {
        assert_matches!(
            &tokenize_str("a>>b")?[..],
            [t1 @ Token::Word(_, _), t2 @ Token::Operator(_, _), t3 @ Token::Word(_, _)] if
                t1.to_str() == "a" &&
                t2.to_str() == ">>" &&
                t3.to_str() == "b"
        );
        Ok(())
    }

    #[test]
    fn tokenize_comment() -> Result<()> {
        let tokens = tokenize_str(
            r#"a #comment
"#,
        )?;
        assert_matches!(
            &tokens[..],
            [t1 @ Token::Word(_, _), t2 @ Token::Operator(_, _)] if
                t1.to_str() == "a" &&
                t2.to_str() == "\n"
        );
        Ok(())
    }

    #[test]
    fn tokenize_comment_at_eof() -> Result<()> {
        assert_matches!(
            &tokenize_str(r#"a #comment"#)?[..],
            [t1 @ Token::Word(_, _)] if t1.to_str() == "a"
        );
        Ok(())
    }

    #[test]
    fn tokenize_here_doc() -> Result<()> {
        let tokens = tokenize_str(
            r#"cat <<HERE
SOMETHING
HERE
"#,
        )?;
        assert_matches!(
            &tokens[..],
            [t1 @ Token::Word(_, _),
             t2 @ Token::Operator(_, _),
             t3 @ Token::Word(_, _),
             t4 @ Token::Operator(_, _),
             t5 @ Token::Word(_, _)] if
                t1.to_str() == "cat" &&
                t2.to_str() == "<<" &&
                t3.to_str() == "HERE" &&
                t4.to_str() == "\n" &&
                t5.to_str() == "SOMETHING\n"
        );
        Ok(())
    }

    #[test]
    fn tokenize_here_doc_with_tab_removal() -> Result<()> {
        let tokens = tokenize_str(
            r#"cat <<-HERE
	SOMETHING
	HERE
"#,
        )?;
        assert_matches!(
            &tokens[..],
            [t1 @ Token::Word(_, _),
             t2 @ Token::Operator(_, _),
             t3 @ Token::Word(_, _),
             t4 @ Token::Operator(_, _),
             t5 @ Token::Word(_, _)] if
                t1.to_str() == "cat" &&
                t2.to_str() == "<<-" &&
                t3.to_str() == "HERE" &&
                t4.to_str() == "\n" &&
                t5.to_str() == "SOMETHING\n"
        );
        Ok(())
    }

    #[test]
    fn tokenize_unterminated_here_doc() -> Result<()> {
        let result = tokenize_str(
            r#"cat <<HERE
SOMETHING
"#,
        );
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn tokenize_missing_here_tag() -> Result<()> {
        let result = tokenize_str(
            r"cat <<
",
        );
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn tokenize_simple_backquote() -> Result<()> {
        assert_matches!(
            &tokenize_str(r#"echo `echo hi`"#)?[..],
            [t1 @ Token::Word(_, _), t2 @ Token::Word(_, _)] if
                t1.to_str() == "echo" &&
                t2.to_str() == "`echo hi`"
        );
        Ok(())
    }

    #[test]
    fn tokenize_backquote_with_escape() -> Result<()> {
        assert_matches!(
            &tokenize_str(r"echo `echo\`hi`")?[..],
            [t1 @ Token::Word(_, _), t2 @ Token::Word(_, _)] if
                t1.to_str() == "echo" &&
                t2.to_str() == r"`echo\`hi`"
        );
        Ok(())
    }

    #[test]
    fn tokenize_command_substitution() -> Result<()> {
        assert_matches!(
            &tokenize_str("a$(echo hi)b c")?[..],
            [t1 @ Token::Word(_, _), t2 @ Token::Word(_, _)] if
                t1.to_str() == "a$(echo hi)b" &&
                t2.to_str() == "c"
        );
        Ok(())
    }

    #[test]
    fn tokenize_unbraced_parameter_expansion() -> Result<()> {
        assert_matches!(
            &tokenize_str("$x")?[..],
            [t1 @ Token::Word(_, _)] if t1.to_str() == "$x"
        );
        assert_matches!(
            &tokenize_str("a$x")?[..],
            [t1 @ Token::Word(_, _)] if t1.to_str() == "a$x"
        );
        Ok(())
    }

    #[test]
    fn tokenize_braced_parameter_expansion() -> Result<()> {
        assert_matches!(
            &tokenize_str("${x}")?[..],
            [t1 @ Token::Word(_, _)] if t1.to_str() == "${x}"
        );
        assert_matches!(
            &tokenize_str("a${x}b")?[..],
            [t1 @ Token::Word(_, _)] if t1.to_str() == "a${x}b"
        );
        Ok(())
    }

    #[test]
    fn tokenize_braced_parameter_expansion_with_escaping() -> Result<()> {
        assert_matches!(
            &tokenize_str(r"a${x\}}b")?[..],
            [t1 @ Token::Word(_, _)] if t1.to_str() == r"a${x\}}b"
        );
        Ok(())
    }

    #[test]
    fn tokenize_whitespace() -> Result<()> {
        assert_matches!(
            &tokenize_str("1 2 3")?[..],
            [t1 @ Token::Word(_, _), t2 @ Token::Word(_, _), t3 @ Token::Word(_, _)] if
                t1.to_str() == "1" &&
                t2.to_str() == "2" &&
                t3.to_str() == "3"
        );
        Ok(())
    }

    #[test]
    fn tokenize_escaped_whitespace() -> Result<()> {
        assert_matches!(
            &tokenize_str(r"1\ 2 3")?[..],
            [t1 @ Token::Word(_, _), t2 @ Token::Word(_, _)] if
                t1.to_str() == r"1\ 2" &&
                t2.to_str() == "3"
        );
        Ok(())
    }

    #[test]
    fn tokenize_single_quote() -> Result<()> {
        assert_matches!(
            &tokenize_str(r"x'a b'y")?[..],
            [t1 @ Token::Word(_, _)] if
                t1.to_str() == r"x'a b'y"
        );
        Ok(())
    }

    #[test]
    fn tokenize_double_quote() -> Result<()> {
        assert_matches!(
            &tokenize_str(r#"x"a b"y"#)?[..],
            [t1 @ Token::Word(_, _)] if
                t1.to_str() == r#"x"a b"y"#
        );
        Ok(())
    }
}
