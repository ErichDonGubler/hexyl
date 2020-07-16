pub(crate) mod input;
pub mod squeezer;

pub use input::*;

use std::convert::TryFrom;
use std::io::{self, Read, Write};

use ansi_term::Color;
use ansi_term::Color::Fixed;

use thiserror::Error as ThisError;

use crate::squeezer::{SqueezeAction, Squeezer};

const BUFFER_SIZE: usize = 256;

const COLOR_NULL: Color = Fixed(242); // grey
const COLOR_OFFSET: Color = Fixed(242); // grey
const COLOR_ASCII_PRINTABLE: Color = Color::Cyan;
const COLOR_ASCII_WHITESPACE: Color = Color::Green;
const COLOR_ASCII_OTHER: Color = Color::Purple;
const COLOR_NONASCII: Color = Color::Yellow;

pub enum ByteCategory {
    Null,
    AsciiPrintable,
    AsciiWhitespace,
    AsciiOther,
    NonAscii,
}

#[derive(Copy, Clone)]
struct Byte(u8);

#[derive(Clone, Copy, Debug)]
pub struct WindowSize(u16);

#[derive(Debug, ThisError)]
#[error("value is not divisible by 2")]
pub struct WindowSizeError;

impl WindowSize {
    pub fn new(n: u16) -> Result<Self, WindowSizeError> {
        if n % 2 == 0 {
            Ok(Self(n))
        } else {
            Err(WindowSizeError)
        }
    }

    pub fn into_inner(self) -> u16 {
        let Self(n) = self;
        n
    }

    fn half_and_full<T1, T2>(self) -> (T1, T2)
    where
        T1: From<u16>,
        T2: From<u16>,
    {
        (self.half(), self.full())
    }

    fn half<T>(self) -> T
    where
        T: From<u16>,
    {
        let Self(n) = self;
        (n / 2).into()
    }

    fn full<T>(self) -> T
    where
        T: From<u16>,
    {
        let Self(n) = self;
        n.into()
    }
}

impl Byte {
    fn category(self) -> ByteCategory {
        if self.0 == 0x00 {
            ByteCategory::Null
        } else if self.0.is_ascii_graphic() {
            ByteCategory::AsciiPrintable
        } else if self.0.is_ascii_whitespace() {
            ByteCategory::AsciiWhitespace
        } else if self.0.is_ascii() {
            ByteCategory::AsciiOther
        } else {
            ByteCategory::NonAscii
        }
    }

    fn color(self) -> &'static Color {
        use crate::ByteCategory::*;

        match self.category() {
            Null => &COLOR_NULL,
            AsciiPrintable => &COLOR_ASCII_PRINTABLE,
            AsciiWhitespace => &COLOR_ASCII_WHITESPACE,
            AsciiOther => &COLOR_ASCII_OTHER,
            NonAscii => &COLOR_NONASCII,
        }
    }

    fn as_char(self) -> char {
        use crate::ByteCategory::*;

        match self.category() {
            Null => '0',
            AsciiPrintable => self.0 as char,
            AsciiWhitespace if self.0 == 0x20 => ' ',
            AsciiWhitespace => '_',
            AsciiOther => '•',
            NonAscii => '×',
        }
    }
}

struct BorderElements {
    left_corner: char,
    horizontal_line: char,
    column_separator: char,
    right_corner: char,
}

pub enum BorderStyle {
    Unicode,
    Ascii,
    None,
}

impl BorderStyle {
    fn header_elems(&self) -> Option<BorderElements> {
        match self {
            BorderStyle::Unicode => Some(BorderElements {
                left_corner: '┌',
                horizontal_line: '─',
                column_separator: '┬',
                right_corner: '┐',
            }),
            BorderStyle::Ascii => Some(BorderElements {
                left_corner: '+',
                horizontal_line: '-',
                column_separator: '+',
                right_corner: '+',
            }),
            BorderStyle::None => None,
        }
    }

    fn footer_elems(&self) -> Option<BorderElements> {
        match self {
            BorderStyle::Unicode => Some(BorderElements {
                left_corner: '└',
                horizontal_line: '─',
                column_separator: '┴',
                right_corner: '┘',
            }),
            BorderStyle::Ascii => Some(BorderElements {
                left_corner: '+',
                horizontal_line: '-',
                column_separator: '+',
                right_corner: '+',
            }),
            BorderStyle::None => None,
        }
    }

    fn outer_sep(&self) -> char {
        match self {
            BorderStyle::Unicode => '│',
            BorderStyle::Ascii => '|',
            BorderStyle::None => ' ',
        }
    }

    fn inner_sep(&self) -> char {
        match self {
            BorderStyle::Unicode => '┊',
            BorderStyle::Ascii => '|',
            BorderStyle::None => ' ',
        }
    }
}

pub struct Printer<'a, Writer: Write> {
    idx: u64,
    /// The raw bytes used as input for the current line.
    raw_line: Vec<u8>,
    /// The buffered line built with each byte, ready to print to writer.
    buffer_line: Vec<u8>,
    writer: &'a mut Writer,
    show_color: bool,
    border_style: BorderStyle,
    header_was_printed: bool,
    byte_hex_table: Vec<String>,
    byte_char_table: Vec<String>,
    squeezer: Squeezer,
    display_offset: u64,
    window_size: WindowSize,
}

impl<'a, Writer: Write> Printer<'a, Writer> {
    pub fn new(
        writer: &'a mut Writer,
        show_color: bool,
        border_style: BorderStyle,
        use_squeeze: bool,
    ) -> Printer<'a, Writer> {
        Printer {
            idx: 1,
            raw_line: vec![],
            buffer_line: vec![],
            writer,
            show_color,
            border_style,
            header_was_printed: false,
            byte_hex_table: (0u8..=u8::max_value())
                .map(|i| {
                    let byte_hex = format!("{:02x} ", i);
                    if show_color {
                        Byte(i).color().paint(byte_hex).to_string()
                    } else {
                        byte_hex
                    }
                })
                .collect(),
            byte_char_table: (0u8..=u8::max_value())
                .map(|i| {
                    let byte_char = format!("{}", Byte(i).as_char());
                    if show_color {
                        Byte(i).color().paint(byte_char).to_string()
                    } else {
                        byte_char
                    }
                })
                .collect(),
            squeezer: Squeezer::new(use_squeeze),
            display_offset: 0,
            window_size: WindowSize::new(16).unwrap(),
        }
    }

    pub fn display_offset(&mut self, display_offset: u64) -> &mut Self {
        self.display_offset = display_offset;
        self
    }

    pub fn window_size(&mut self, window_size: WindowSize) -> &mut Self {
        self.window_size = window_size;
        self
    }

    fn print_border_elements<W>(
        writer: &mut W,
        window_size: WindowSize,
        border_elements: BorderElements,
    ) where
        W: Write,
    {
        let half_window_size =
            usize::try_from(window_size.half::<u16>()).expect("window size doesn't fit into usize");
        let h = border_elements.horizontal_line;
        let side_segment = h.to_string().repeat(half_window_size);
        let main_segment = h.to_string().repeat(
            half_window_size
                .checked_mul(3)
                .unwrap()
                .checked_add(1)
                .unwrap(),
        );

        let _ = writeln!(
            writer,
            "{l}{side_segment}{c}\
            {main_segment}{c}{main_segment}\
            {c}{side_segment}{c}{side_segment}{r}",
            l = border_elements.left_corner,
            c = border_elements.column_separator,
            r = border_elements.right_corner,
            side_segment = side_segment,
            main_segment = main_segment
        );
    }

    pub fn header(&mut self) {
        let &mut Self {
            ref border_style,
            window_size,
            ref mut writer,
            ..
        } = self;
        border_style
            .header_elems()
            .map(|bes| Self::print_border_elements(writer, window_size, bes));
    }

    pub fn footer(&mut self) {
        let &mut Self {
            ref border_style,
            window_size,
            ref mut writer,
            ..
        } = self;
        border_style
            .footer_elems()
            .map(|bes| Self::print_border_elements(writer, window_size, bes));
    }

    fn print_position_indicator(&mut self) {
        if !self.header_was_printed {
            self.header();
            self.header_was_printed = true;
        }

        let style = COLOR_OFFSET.normal();
        let byte_index = format!(
            "{:0alignment$x}",
            self.idx - 1 + self.display_offset,
            alignment = self.window_size.half()
        );
        let formatted_string = if self.show_color {
            format!("{}", style.paint(byte_index))
        } else {
            byte_index
        };
        let _ = write!(
            &mut self.buffer_line,
            "{}{}{} ",
            self.border_style.outer_sep(),
            formatted_string,
            self.border_style.outer_sep()
        );
    }

    pub fn print_byte(&mut self, b: u8) -> io::Result<()> {
        let (half_window_boundary, full_window_boundary) =
            self.window_size.half_and_full::<u64, u64>();

        if self.idx % full_window_boundary == 1 {
            self.print_position_indicator();
        }

        write!(&mut self.buffer_line, "{}", self.byte_hex_table[b as usize])?;
        self.raw_line.push(b);

        self.squeezer.process(self.window_size, b, self.idx);

        match self.idx % full_window_boundary {
            n if n == half_window_boundary => {
                let _ = write!(&mut self.buffer_line, "{} ", self.border_style.inner_sep());
            }
            0 => {
                self.print_textline()?;
            }
            _ => {}
        }

        self.idx += 1;

        Ok(())
    }

    pub fn print_textline(&mut self) -> io::Result<()> {
        assert!(
            usize::try_from(self.window_size.half::<u16>())
                .ok()
                .and_then(|ws| ws.checked_mul(3).and_then(|s| s.checked_add(1)))
                .is_some(),
            "window size calculations exceed usize range",
        );

        let half_window_size = usize::try_from(self.window_size.half::<u16>()).unwrap();

        let len = self.raw_line.len();

        if len == 0 {
            if self.squeezer.active() {
                self.print_position_indicator();
                let _ = writeln!(
                    &mut self.buffer_line,
                    "{0:1$}{4}{0:2$}{5}{0:3$}{4}{0:3$}{5}",
                    "",
                    half_window_size * 3,
                    half_window_size * 3 + 1,
                    half_window_size,
                    self.border_style.inner_sep(),
                    self.border_style.outer_sep(),
                );
                self.writer.write_all(&self.buffer_line)?;
            }
            return Ok(());
        }

        let squeeze_action = self.squeezer.action();

        if squeeze_action != SqueezeAction::Delete {
            if len < half_window_size {
                let _ = write!(
                    &mut self.buffer_line,
                    "{0:1$}{3}{0:2$}{4}",
                    "",
                    3 * (half_window_size - len),
                    half_window_size * 3 + 1,
                    self.border_style.inner_sep(),
                    self.border_style.outer_sep(),
                );
            } else {
                let _ = write!(
                    &mut self.buffer_line,
                    "{0:1$}{2}",
                    "",
                    3 * (half_window_size * 2 - len),
                    self.border_style.outer_sep()
                );
            }

            let mut idx = 1;
            for &b in self.raw_line.iter() {
                let _ = write!(
                    &mut self.buffer_line,
                    "{}",
                    self.byte_char_table[b as usize]
                );

                if idx == half_window_size {
                    let _ = write!(&mut self.buffer_line, "{}", self.border_style.inner_sep());
                }

                idx += 1;
            }

            if len < half_window_size {
                let _ = writeln!(
                    &mut self.buffer_line,
                    "{0:1$}{3}{0:2$}{4}",
                    "",
                    half_window_size - len,
                    half_window_size,
                    self.border_style.inner_sep(),
                    self.border_style.outer_sep(),
                );
            } else {
                let _ = writeln!(
                    &mut self.buffer_line,
                    "{0:1$}{2}",
                    "",
                    half_window_size * 2 - len,
                    self.border_style.outer_sep()
                );
            }
        }

        match squeeze_action {
            SqueezeAction::Print => {
                self.buffer_line.clear();
                let style = COLOR_OFFSET.normal();
                let asterisk = if self.show_color {
                    format!("{}", style.paint("*"))
                } else {
                    String::from("*")
                };
                let _ = writeln!(
                    &mut self.buffer_line,
                    "{5}{0}{1:2$}{5}{1:3$}{6}{1:3$}{5}{1:4$}{6}{1:4$}{5}",
                    asterisk,
                    "",
                    half_window_size - 1,
                    half_window_size * 3 + 1,
                    half_window_size,
                    self.border_style.outer_sep(),
                    self.border_style.inner_sep(),
                );
            }
            SqueezeAction::Delete => self.buffer_line.clear(),
            SqueezeAction::Ignore => (),
        }

        self.writer.write_all(&self.buffer_line)?;

        self.raw_line.clear();
        self.buffer_line.clear();

        self.squeezer.advance();

        Ok(())
    }

    pub fn header_was_printed(&self) -> bool {
        self.header_was_printed
    }

    /// Loop through the given `Reader`, printing until the `Reader` buffer
    /// is exhausted.
    pub fn print_all<Reader: Read>(
        &mut self,
        mut reader: Reader,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut buffer = [0; BUFFER_SIZE];
        'mainloop: loop {
            let size = reader.read(&mut buffer)?;
            if size == 0 {
                break;
            }

            for b in &buffer[..size] {
                let res = self.print_byte(*b);

                if res.is_err() {
                    // Broken pipe
                    break 'mainloop;
                }
            }
        }

        // Finish last line
        self.print_textline().ok();
        if !self.header_was_printed() {
            self.header();
        }
        self.footer();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::str;

    use super::*;

    fn assert_print_all_output<Reader: Read>(input: Reader, expected_string: String) -> () {
        let mut output = vec![];
        let mut printer = Printer::new(&mut output, false, BorderStyle::Unicode, true);

        printer.print_all(input).unwrap();

        let actual_string: &str = str::from_utf8(&output).unwrap();
        assert_eq!(actual_string, expected_string)
    }

    #[test]
    fn empty_file_passes() {
        let input = io::empty();
        let expected_string = "\
┌────────┬─────────────────────────┬─────────────────────────┬────────┬────────┐
└────────┴─────────────────────────┴─────────────────────────┴────────┴────────┘
"
        .to_owned();
        assert_print_all_output(input, expected_string);
    }

    #[test]
    fn short_input_passes() {
        let input = io::Cursor::new(b"spam");
        let expected_string = "\
┌────────┬─────────────────────────┬─────────────────────────┬────────┬────────┐
│00000000│ 73 70 61 6d             ┊                         │spam    ┊        │
└────────┴─────────────────────────┴─────────────────────────┴────────┴────────┘
"
        .to_owned();
        assert_print_all_output(input, expected_string);
    }

    #[test]
    fn display_offset() {
        let input = io::Cursor::new(b"spamspamspamspamspam");
        let expected_string = "\
┌────────┬─────────────────────────┬─────────────────────────┬────────┬────────┐
│deadbeef│ 73 70 61 6d 73 70 61 6d ┊ 73 70 61 6d 73 70 61 6d │spamspam┊spamspam│
│deadbeff│ 73 70 61 6d             ┊                         │spam    ┊        │
└────────┴─────────────────────────┴─────────────────────────┴────────┴────────┘
"
        .to_owned();

        let mut output = vec![];
        let mut printer: Printer<Vec<u8>> =
            Printer::new(&mut output, false, BorderStyle::Unicode, true);
        printer.display_offset(0xdeadbeef);

        printer.print_all(input).unwrap();

        let actual_string: &str = str::from_utf8(&output).unwrap();
        assert_eq!(actual_string, expected_string)
    }
}
