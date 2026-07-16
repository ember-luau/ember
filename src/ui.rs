use clap::builder::styling;
use inquire::ui::{Color, ErrorMessageRenderConfig, RenderConfig, StyleSheet, Styled};

/// Theme accent (#e61048), shared by the prompt UI, help output, and errors.
pub const ACCENT: (u8, u8, u8) = (0xe6, 0x10, 0x48);

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
    use crossterm::style::Stylize;

    let (r, g, b) = ACCENT;
    let accent = crossterm::style::Color::Rgb { r, g, b };
    eprintln!("{}", format!("✗ {message}").with(accent));
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
