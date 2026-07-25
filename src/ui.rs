use std::time::Duration;

use crate::error::Error;
use clap::builder::styling;
use crossterm::style::Stylize;
use inquire::ui::{Color, ErrorMessageRenderConfig, RenderConfig, StyleSheet, Styled};

/// Theme accent (#e61048), shared by the prompt UI, help output, and errors.
pub const ACCENT: (u8, u8, u8) = (0xe6, 0x10, 0x48);

fn accent() -> crossterm::style::Color {
    let (r, g, b) = ACCENT;
    crossterm::style::Color::Rgb { r, g, b }
}

/// Styles clap's help/usage/error output to match the prompt theme:
/// regular section headers, accent command/flag names, dimmed placeholders.
pub fn help_styles() -> styling::Styles {
    let (r, g, b) = ACCENT;
    let accent = styling::Style::new().fg_color(Some(styling::RgbColor(r, g, b).into()));
    let dimmed = styling::Style::new().fg_color(Some(styling::AnsiColor::BrightBlack.into()));

    styling::Styles::styled()
        // Section headers ("Usage:", "Commands:", "Options:")
        .header(styling::Style::new())
        .usage(styling::Style::new())
        // Command, flag, and value names as typed literally
        .literal(accent)
        // Placeholder metavariables like <COMMAND>
        .placeholder(dimmed)
        // clap's own parse errors and valid/invalid value hints
        .error(accent)
        .valid(accent)
        .invalid(dimmed)
}

/// Prints an error line to stderr as "✗ message" in the accent color.
pub fn print_error(message: &str) {
    eprintln!("{}", format!("✗ {message}").with(accent()));
}

/// The "✓ message" line print_success emits, as a string so progress bars
/// can `bar.println(...)` it without fighting stdout.
pub fn success_line(message: &str) -> String {
    format!("{} {message}", "✓".with(accent()))
}

/// Prints a progress line as an accent "✓" followed by the message.
pub fn print_success(message: &str) {
    println!("{}", success_line(message));
}

/// Prints a line to stdout while `bar` is live. `ProgressBar::println` routes
/// through the bar's draw target, which writes to stderr on a terminal and
/// swallows the line entirely when output is piped; suspending instead keeps
/// the line on stdout in both worlds.
pub fn bar_println(bar: &indicatif::ProgressBar, line: &str) {
    bar.suspend(|| println!("{line}"));
}

/// A `{msg} ━━━╸─── {pos}/{len}` bar with the filled portion in the accent
/// color. indicatif templates only speak ANSI-16/256, so the exact accent RGB
/// is smuggled in as crossterm escapes around the bar placeholder — literal
/// template text passes through untouched (and width measurement is
/// escape-aware).
pub fn progress_bar(len: u64) -> indicatif::ProgressBar {
    let template = format!("{{msg}} {} {{pos}}/{{len}}", "{bar:40}".with(accent()));
    let style = indicatif::ProgressStyle::with_template(&template)
        .expect("progress bar template is valid")
        .progress_chars("━╸─");

    let bar = indicatif::ProgressBar::new(len);
    bar.set_style(style);
    bar
}

/// An accent spinner with a message, ticking on its own. The caller finishes
/// or clears it.
pub fn spinner(message: &str) -> indicatif::ProgressBar {
    let template = format!("{} {{msg}}", "{spinner}".with(accent()));
    let style =
        indicatif::ProgressStyle::with_template(&template).expect("spinner template is valid");

    let spinner = indicatif::ProgressBar::new_spinner();
    spinner.set_style(style);
    spinner.set_message(message.to_string());
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner
}

/// Runs `work` under a spinner. The spinner is always cleared first, so an
/// error message never lands on top of a live one.
pub fn with_spinner<T>(message: &str, work: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    let spinner = spinner(message);
    let result = work();
    spinner.finish_and_clear();
    result
}

/// Runs `work` with a progress bar of `len` steps, clearing it afterwards
/// whether the work succeeded or not.
pub fn with_progress<T>(
    len: u64,
    work: impl FnOnce(&indicatif::ProgressBar) -> Result<T, Error>,
) -> Result<T, Error> {
    let bar = progress_bar(len);
    let result = work(&bar);
    bar.finish_and_clear();
    result
}

/// "" for one, "s" for any other count, for summary lines.
pub fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// "<1ms", "142ms", "1.42s", or "1m 12s" depending on magnitude.
pub fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1 {
        "<1ms".to_string()
    } else if millis < 1_000 {
        format!("{millis}ms")
    } else if millis < 60_000 {
        format!("{}.{:02}s", millis / 1_000, millis % 1_000 / 10)
    } else {
        let secs = duration.as_secs();
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// Prints a dimmed "Done in 142ms" line.
pub fn print_elapsed(duration: Duration) {
    println!(
        "{}",
        format!("Done in {}", format_duration(duration)).dark_grey()
    );
}

pub fn render_config() -> RenderConfig<'static> {
    let (r, g, b) = ACCENT;
    let accent = Color::rgb(r, g, b);

    RenderConfig::default_colored()
        // The "?" in front of a pending question and the "✓" once answered
        .with_prompt_prefix(Styled::new("?").with_fg(Color::DarkGrey))
        .with_answered_prompt_prefix(Styled::new("✓").with_fg(accent))
        // The value you type / the answer shown after confirming
        .with_text_input(StyleSheet::new().with_fg(Color::White))
        .with_answer(StyleSheet::new().with_fg(accent))
        // The dimmed description line below each prompt
        .with_help_message(StyleSheet::new().with_fg(Color::DarkGrey))
        // The inline "(default)" hint shown when a guessed default is available
        .with_default_value(StyleSheet::new().with_fg(Color::DarkGrey))
        // The ">" cursor and the highlighted row in select lists
        .with_highlighted_option_prefix(Styled::new(">").with_fg(accent))
        .with_selected_option(Some(StyleSheet::new().with_fg(accent)))
        // The validation error line shown below a prompt (default prefix is "#")
        .with_error_message(
            ErrorMessageRenderConfig::default_colored()
                .with_prefix(Styled::new("✗").with_fg(accent))
                .with_message(StyleSheet::new().with_fg(accent)),
        )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::format_duration;

    #[test]
    fn sub_millisecond_floors_to_less_than_one() {
        assert_eq!(format_duration(Duration::ZERO), "<1ms");
        assert_eq!(format_duration(Duration::from_micros(999)), "<1ms");
    }

    #[test]
    fn milliseconds_up_to_a_second() {
        assert_eq!(format_duration(Duration::from_millis(1)), "1ms");
        assert_eq!(format_duration(Duration::from_millis(142)), "142ms");
        assert_eq!(format_duration(Duration::from_millis(999)), "999ms");
    }

    #[test]
    fn seconds_with_two_decimals() {
        assert_eq!(format_duration(Duration::from_millis(1_000)), "1.00s");
        assert_eq!(format_duration(Duration::from_millis(1_420)), "1.42s");
        assert_eq!(format_duration(Duration::from_millis(59_999)), "59.99s");
    }

    #[test]
    fn minutes_and_seconds() {
        assert_eq!(format_duration(Duration::from_secs(60)), "1m 0s");
        assert_eq!(format_duration(Duration::from_secs(72)), "1m 12s");
        assert_eq!(format_duration(Duration::from_secs(3_599)), "59m 59s");
    }
}
