#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TerminalSymbol {
    BoxDrawing(char),
    BlockElement(char),
    Braille(u8),
    Powerline(char),
    LegacySextant(u8),
}

pub fn terminal_symbol(text: &str) -> Option<TerminalSymbol> {
    let mut characters = text.chars();
    let character = characters.next()?;
    if characters.next().is_some() {
        return None;
    }

    match character as u32 {
        0x2500..=0x257f => Some(TerminalSymbol::BoxDrawing(character)),
        0x2580..=0x259f => Some(TerminalSymbol::BlockElement(character)),
        0x2800..=0x28ff => Some(TerminalSymbol::Braille(
            u8::try_from(character as u32 - 0x2800).ok()?,
        )),
        0xe0b0..=0xe0bf => Some(TerminalSymbol::Powerline(character)),
        0x1fb00..=0x1fb3b => Some(TerminalSymbol::LegacySextant(
            u8::try_from(character as u32 - 0x1fb00).ok()?,
        )),
        _ => None,
    }
}
