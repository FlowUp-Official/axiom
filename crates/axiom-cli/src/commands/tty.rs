//! TTY detection. Presentation-only: it never changes what files are touched.

use std::io::IsTerminal;

/// Whether stderr is attached to a terminal. Diagnostics are rendered on
/// stderr, so that is the stream we probe.
pub fn is_tty() -> bool {
    std::io::stderr().is_terminal()
}
