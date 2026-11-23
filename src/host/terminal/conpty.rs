use std::io::Write;

use vte::{Params, Parser, Perform};

const MAX_REPLIES_PER_CHUNK: usize = 32;

#[derive(Default)]
pub(super) struct Queries {
    parser: Parser,
}

impl Queries {
    /// Feed the screen in order so each response reports the cursor at the query.
    pub(super) fn process(&mut self, screen: &mut vt100::Parser, bytes: &[u8]) -> Vec<u8> {
        let mut reply = Probe::default();
        let mut responses = Vec::new();
        let mut start = 0;
        let mut remaining = MAX_REPLIES_PER_CHUNK;
        for (index, &byte) in bytes.iter().enumerate() {
            self.parser.advance(&mut reply, byte);
            let Some(query) = reply.0.take() else {
                continue;
            };
            if remaining == 0 {
                continue;
            }
            remaining -= 1;
            let end = index + 1;
            screen.process(&bytes[start..end]);
            start = end;
            match query {
                Query::Cursor { private } => {
                    let (row, col) = screen.screen().cursor_position();
                    let prefix = if private { "?" } else { "" };
                    write!(responses, "\x1b[{prefix}{};{}R", row + 1, col + 1)
                        .expect("writing to a Vec cannot fail");
                }
                Query::PrimaryAttributes => responses.extend_from_slice(b"\x1b[?1;2c"),
            }
        }
        screen.process(&bytes[start..]);
        responses
    }
}

enum Query {
    Cursor { private: bool },
    PrimaryAttributes,
}

#[derive(Default)]
struct Probe(Option<Query>);

impl Perform for Probe {
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let mut parameters = params.iter();
        let parameter = parameters.next();
        if parameters.next().is_some() {
            return;
        }
        self.0 = match (action, intermediates, parameter) {
            ('n', [], Some([6])) => Some(Query::Cursor { private: false }),
            ('n', [b'?'], Some([6])) => Some(Query::Cursor { private: true }),
            ('c', [], None | Some([0])) => Some(Query::PrimaryAttributes),
            _ => None,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_REPLIES_PER_CHUNK, Queries};

    #[test]
    fn replies_at_each_query_position_across_chunks() {
        let mut queries = Queries::default();
        let mut screen = vt100::Parser::new(24, 80, 0);
        assert!(queries.process(&mut screen, b"abc\x1b[6").is_empty());
        assert_eq!(
            queries.process(&mut screen, b"n\r\nxy\x1b[?6n\x1b[c\x1b[0c"),
            b"\x1b[1;4R\x1b[?2;3R\x1b[?1;2c\x1b[?1;2c"
        );
        assert_eq!(screen.screen().cursor_position(), (1, 2));
    }

    #[test]
    fn ignores_other_commands_and_string_content() {
        let mut queries = Queries::default();
        let mut screen = vt100::Parser::new(24, 80, 0);
        assert!(queries.process(&mut screen, b"\x1b]0;[6n [c").is_empty());
        assert!(
            queries
                .process(
                    &mut screen,
                    b"\x07\x1bPq[6n[c\x1b\\\x1b[5n\x1b[>c\x1b[6;0n\x1b[6:0n\x1b[?1;2c"
                )
                .is_empty()
        );
        assert_eq!(queries.process(&mut screen, b"\x1b[c"), b"\x1b[?1;2c");
    }

    #[test]
    fn bounds_replies_without_losing_screen_updates() {
        let mut queries = Queries::default();
        let mut screen = vt100::Parser::new(24, 80, 0);
        let mut bytes = b"\x1b[c".repeat(MAX_REPLIES_PER_CHUNK + 1);
        bytes.extend_from_slice(b"visible");
        assert_eq!(
            queries.process(&mut screen, &bytes),
            b"\x1b[?1;2c".repeat(MAX_REPLIES_PER_CHUNK)
        );
        assert_eq!(screen.screen().cursor_position(), (0, 7));
    }
}
