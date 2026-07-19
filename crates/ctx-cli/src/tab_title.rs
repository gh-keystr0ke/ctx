use std::io::{self, IsTerminal, Write};

/// Sets the terminal tab/window title via the xterm `OSC 0` escape sequence
/// (`\x1b]0;<title>\x07`) -- widely supported (iTerm2, kitty, Ghostty,
/// Alacritty, GNOME Terminal, Windows Terminal, VS Code's integrated
/// terminal), unlike any single terminal's own proprietary bitmap-icon
/// protocol. Lets a long `ctx --auto` batch show its `[position/total]`
/// progress in the tab itself, visible at a glance across several open
/// tabs without switching to the one actually running it.
///
/// A no-op when stderr isn't a terminal -- CI logs and redirected output
/// should never receive raw escape bytes.
pub fn set_title(title: &str) {
    if !io::stderr().is_terminal() {
        return;
    }
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(title_sequence(title).as_bytes());
    let _ = stderr.flush();
}

/// The raw xterm `OSC 0` bytes for setting the window/tab title to `title`,
/// split out from [`set_title`] purely so it's testable without a real
/// terminal attached.
fn title_sequence(title: &str) -> String {
    format!("\x1b]0;{title}\x07")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_sequence_wraps_the_title_in_osc_0() {
        assert_eq!(
            title_sequence("ctx verify --auto"),
            "\x1b]0;ctx verify --auto\x07"
        );
    }
}
