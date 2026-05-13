use std::io::{self, Write};

/// Print a formatted phase header banner to stdout.
pub fn print_phase_header(phase: u32, title: &str) {
    let bar = "─".repeat(66);
    println!("\n┌{bar}┐");
    println!("│  THERMITE — Phase {phase}: {title:<46}│");
    println!("└{bar}┘");
}

/// Print a formatted info box with a title and body lines.
pub fn print_info_box(title: &str, lines: &[&str]) {
    let width = 66usize;
    let bar = "─".repeat(width);
    println!("┌{bar}┐");
    println!("│  {title:<64}│");
    println!("├{bar}┤");
    for line in lines {
        // Word-wrap long lines at width - 4 characters.
        for chunk in wrap_line(line, width - 4) {
            println!("│  {chunk:<64}│");
        }
    }
    println!("└{bar}┘");
}

/// Wrap a single line to at most `max_width` characters per chunk.
fn wrap_line(line: &str, max_width: usize) -> Vec<String> {
    if line.len() <= max_width {
        return vec![line.to_owned()];
    }
    let mut chunks = Vec::new();
    let mut remaining = line;
    while remaining.len() > max_width {
        // Break at the last space within max_width, or hard-break if no space.
        let split_at = remaining[..max_width]
            .rfind(' ')
            .map(|i| i + 1)
            .unwrap_or(max_width);
        chunks.push(remaining[..split_at].to_owned());
        remaining = &remaining[split_at..];
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_owned());
    }
    chunks
}

/// Prompt the user to press Enter to continue. Returns when Enter is pressed.
pub fn prompt_continue(message: &str) {
    print!("\n  {message} [Press Enter to continue] ");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
}

/// Prompt the user with a yes/no question. Returns `true` if the user answers
/// `y` or `yes` (case-insensitive). Any other input (including Enter) returns
/// `false`.
pub fn prompt_yes_no(question: &str) -> bool {
    print!("\n  {question} [y/N] ");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
    matches!(buf.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Prompt the user for a line of text input.  Returns the trimmed string.
/// Used in phase 9 (C-library removal) to capture exclusion patterns and
/// extra build-depends entries interactively.
pub fn prompt_input(prompt: &str) -> String {
    print!("\n  {prompt} ");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
    buf.trim().to_owned()
}
