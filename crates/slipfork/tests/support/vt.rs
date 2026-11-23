//! A real `(rows × cols)` VT100 screen grid for byte-level terminal tests.
//!
//! The suite never mocks a renderer: it feeds the exact bytes an `openpty` returns and reads the
//! resulting cells. The parser covers what the boot pane emits — `\r`, `\n` (scrolling only at the
//! `DECSTBM` bottom margin), `ESC[r`, `ESC[H`, `ESC[{row};{col}H`, `ESC[2K`, `ESC[K`, `ESC7`/`ESC8`,
//! and wide characters via `console::measure_text_width` — plus `DSR` (`ESC[6n`) requests, which
//! the PTY harness answers so the shipped pane can seed its flow cursor.

#![allow(dead_code)]

const ESC: u8 = 0x1b;

#[derive(Debug, Clone)]
enum State {
    Ground,
    Esc,
    Csi(Vec<u8>),
    Osc,
    OscEsc,
}

/// One parsed terminal screen; rows and columns are 0-based, text is trimmed at the right edge.
#[derive(Debug, Clone)]
pub struct Screen {
    rows: usize,
    cols: usize,
    cells: Vec<Vec<char>>,
    row: usize,
    col: usize,
    scroll_top: usize,
    scroll_bottom: usize,
    saved: Option<(usize, usize)>,
    dsr_requests: usize,
    state: State,
    pending: Vec<u8>,
}

impl Screen {
    /// A blank `rows × cols` screen with the scroll region covering the whole screen.
    #[must_use]
    pub fn new(rows: usize, cols: usize) -> Self {
        assert!(rows > 0 && cols > 0, "a screen needs at least one cell");
        Self {
            rows,
            cols,
            cells: vec![vec![' '; cols]; rows],
            row: 0,
            col: 0,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            saved: None,
            dsr_requests: 0,
            state: State::Ground,
            pending: Vec::new(),
        }
    }

    /// Places `text` at `(row, col)` without wrapping, leaving the cursor at its end.
    pub fn place(&mut self, row: usize, col: usize, text: &str) {
        self.row = row.min(self.rows - 1);
        self.col = col.min(self.cols - 1);
        self.feed(text.as_bytes());
    }

    /// Feeds one arbitrary chunk of real PTY output.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let mut index = 0;
        while index < self.pending.len() {
            let byte = self.pending[index];
            let state = std::mem::replace(&mut self.state, State::Ground);
            match state {
                State::Ground => {
                    let consumed = self.ground(byte, index);
                    if consumed == usize::MAX {
                        // Incomplete UTF-8 at the chunk boundary: re-parse after the next read.
                        break;
                    }
                    index += consumed;
                }
                State::Esc => {
                    index += 1;
                    match byte {
                        b'[' => self.state = State::Csi(Vec::new()),
                        b']' => self.state = State::Osc,
                        b'7' => self.saved = Some((self.row, self.col)),
                        b'8' => {
                            if let Some((row, col)) = self.saved {
                                self.row = row.min(self.rows - 1);
                                self.col = col.min(self.cols - 1);
                            }
                        }
                        // Charset designators `ESC ( B` swallow exactly one more byte.
                        b'(' | b')' | b'*' | b'+' => self.state = State::Esc,
                        _ => {}
                    }
                }
                State::Csi(mut params) => {
                    index += 1;
                    if (0x20..=0x3f).contains(&byte) {
                        params.push(byte);
                        self.state = State::Csi(params);
                    } else {
                        self.dispatch(byte, &params);
                    }
                }
                State::Osc => {
                    index += 1;
                    match byte {
                        0x07 => {}
                        ESC => self.state = State::OscEsc,
                        _ => self.state = State::Osc,
                    }
                }
                State::OscEsc => {
                    index += 1;
                }
            }
        }
        self.pending.drain(..index);
    }

    /// The trimmed text of one row.
    #[must_use]
    pub fn row_text(&self, row: usize) -> String {
        let mut text: String = self.cells[row]
            .iter()
            .filter(|cell| **cell != '\0')
            .collect();
        while text.ends_with(' ') {
            text.pop();
        }
        text
    }

    /// Every row as trimmed text, top to bottom.
    #[must_use]
    pub fn text(&self) -> String {
        (0..self.rows)
            .map(|row| self.row_text(row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The first row whose trimmed text contains `needle`.
    #[must_use]
    pub fn find_row(&self, needle: &str) -> Option<usize> {
        (0..self.rows).find(|row| self.row_text(*row).contains(needle))
    }

    /// The cursor as 0-based `(row, column)`.
    #[must_use]
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// Total `ESC[6n` cursor-position requests seen in the output.
    #[must_use]
    pub fn dsr_requests(&self) -> usize {
        self.dsr_requests
    }

    fn ground(&mut self, byte: u8, index: usize) -> usize {
        match byte {
            ESC => {
                self.state = State::Esc;
                1
            }
            b'\r' => {
                self.col = 0;
                1
            }
            b'\n' => {
                self.linefeed();
                1
            }
            0x08 => {
                self.col = self.col.saturating_sub(1);
                1
            }
            b'\t' => {
                self.col = ((self.col / 8) + 1) * 8;
                self.col = self.col.min(self.cols - 1);
                1
            }
            0x00..=0x1f | 0x7f => 1,
            0x20..=0x7e => {
                self.put_char(char::from(byte));
                1
            }
            _ => match decode_utf8(&self.pending[index..]) {
                Some((character, length)) => {
                    self.put_char(character);
                    length
                }
                None => usize::MAX,
            },
        }
    }

    fn put_char(&mut self, character: char) {
        let width = console::measure_text_width(&character.to_string()).max(1);
        if self.col >= self.cols || (width == 2 && self.col + 1 >= self.cols) {
            self.col = 0;
            self.linefeed();
        }
        self.cells[self.row][self.col] = character;
        if width == 2 && self.col + 1 < self.cols {
            self.cells[self.row][self.col + 1] = '\0';
        }
        self.col = (self.col + width).min(self.cols);
    }

    fn linefeed(&mut self) {
        if self.row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.row < self.rows - 1 {
            self.row += 1;
        }
    }

    fn scroll_up(&mut self, count: usize) {
        for _ in 0..count {
            for row in self.scroll_top..self.scroll_bottom {
                let next = self.cells[row + 1].clone();
                self.cells[row] = next;
            }
            self.cells[self.scroll_bottom].fill(' ');
        }
    }

    fn scroll_down(&mut self, count: usize) {
        for _ in 0..count {
            let mut row = self.scroll_bottom;
            while row > self.scroll_top {
                let previous = self.cells[row - 1].clone();
                self.cells[row] = previous;
                row -= 1;
            }
            self.cells[self.scroll_top].fill(' ');
        }
    }

    fn clear_row(&mut self, row: usize, from: usize, to: usize) {
        let to = to.min(self.cols);
        self.cells[row][from.min(to)..to].fill(' ');
    }

    fn dispatch(&mut self, final_byte: u8, raw: &[u8]) {
        let (private, values) = parse_params(raw);
        match final_byte {
            b'H' | b'f' => {
                self.row = (param(&values, 0, 1) - 1).min(self.rows - 1);
                self.col = (param(&values, 1, 1) - 1).min(self.cols - 1);
            }
            b'r' => {
                if values.is_empty() {
                    self.scroll_top = 0;
                    self.scroll_bottom = self.rows - 1;
                } else {
                    let top = (param(&values, 0, 1) - 1).min(self.rows - 1);
                    let bottom = (param(&values, 1, self.rows) - 1).min(self.rows - 1);
                    if top < bottom {
                        self.scroll_top = top;
                        self.scroll_bottom = bottom;
                    }
                }
            }
            b'K' => match param(&values, 0, 0) {
                0 => self.clear_row(self.row, self.col, self.cols),
                1 => self.clear_row(self.row, 0, self.col + 1),
                _ => self.clear_row(self.row, 0, self.cols),
            },
            b'J' => match param(&values, 0, 0) {
                1 => {
                    for row in 0..self.row {
                        self.clear_row(row, 0, self.cols);
                    }
                    self.clear_row(self.row, 0, self.col + 1);
                }
                2 | 3 => {
                    for row in 0..self.rows {
                        self.clear_row(row, 0, self.cols);
                    }
                    self.row = 0;
                    self.col = 0;
                }
                _ => {
                    self.clear_row(self.row, self.col, self.cols);
                    for row in self.row + 1..self.rows {
                        self.clear_row(row, 0, self.cols);
                    }
                }
            },
            b'n' if !private => {
                if param(&values, 0, 0) == 6 {
                    self.dsr_requests += 1;
                }
            }
            b'A' => self.row = self.row.saturating_sub(param(&values, 0, 1)),
            b'B' => self.row = (self.row + param(&values, 0, 1)).min(self.rows - 1),
            b'C' => self.col = (self.col + param(&values, 0, 1)).min(self.cols),
            b'D' => self.col = self.col.saturating_sub(param(&values, 0, 1)),
            b'G' => self.col = (param(&values, 0, 1) - 1).min(self.cols - 1),
            b'd' => self.row = (param(&values, 0, 1) - 1).min(self.rows - 1),
            b'S' => self.scroll_up(param(&values, 0, 1)),
            b'T' => self.scroll_down(param(&values, 0, 1)),
            _ => {}
        }
    }
}

fn parse_params(raw: &[u8]) -> (bool, Vec<Option<usize>>) {
    let private = raw
        .first()
        .is_some_and(|byte| matches!(byte, b'?' | b'>' | b'<' | b'='));
    let body = if private { &raw[1..] } else { raw };
    let text = String::from_utf8_lossy(body);
    let values = if text.is_empty() {
        Vec::new()
    } else {
        text.split(';')
            .map(|part| part.parse::<usize>().ok())
            .collect()
    };
    (private, values)
}

fn param(values: &[Option<usize>], index: usize, default: usize) -> usize {
    values
        .get(index)
        .copied()
        .flatten()
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn decode_utf8(bytes: &[u8]) -> Option<(char, usize)> {
    let first = bytes[0];
    let length = match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return Some(('\u{fffd}', 1)),
    };
    if bytes.len() < length {
        return None;
    }
    match std::str::from_utf8(&bytes[..length]) {
        Ok(text) => text.chars().next().map(|character| (character, length)),
        Err(_) => Some(('\u{fffd}', 1)),
    }
}
