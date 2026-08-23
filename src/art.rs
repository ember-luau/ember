/*! the logo as ascii art, and the brand gradient it and the help text are
painted with.

regenerate from ember.png with scripts/logo.py if the mark changes. the mark
is a flame above an open crate. its alpha carries every knockout that counts,
the curl inside the flame and the gap between the crate panels, so the art
places none by hand. the two spark diamonds are the one thing dropped: each
covers about one cell at this size, and renders as stray punctuation. */

use crate::ui::{ACCENT, RESET, fg};

/// the logo, painted with the gradient unless `color` is off.
pub fn logo(color: bool) -> String {
    render_gradient(LOGO, color)
}

/** the gradient colour of one row: gold at the top, through the brand accent
in the middle, down to a deep ember.

The hue travels, it is not one colour getting darker. A flame runs gold where
it is hottest and deep red where it is spent, and the mark is a flame above a
crate, so the ramp reads down the artwork the way heat does. A single-hue run
from a pale tint to a dark shade renders the top rows as pastel, which is what
this replaces.

the help text beside the logo uses the same ramp for the same row, so the two
columns read as one object lit from above rather than two things that happen
to sit next to each other. */
pub fn row_color(row: usize, rows: usize) -> (u8, u8, u8) {
    /* the ends stay off pure white and off near black on purpose: the help
    text is painted with this same ramp, and its first and last lines have to
    stay readable on a light terminal as well as a dark one */
    const TOP: (u8, u8, u8) = (0xFF, 0xC2, 0x4A); // gold, the hot tip
    const BOTTOM: (u8, u8, u8) = (0x8C, 0x16, 0x07); // deep ember, the coal

    let last = rows.saturating_sub(1).max(1) as u32;
    let position = row.min(rows.saturating_sub(1)) as u32 * 1000 / last; // 0..=1000
    let mix = |from: u8, to: u8, at: u32, span: u32| -> u8 {
        let from = from as i32;
        let to = to as i32;
        (from + (to - from) * at as i32 / span as i32) as u8
    };

    // two legs, top -> accent for the first half, accent -> bottom for the second
    if position <= 500 {
        (
            mix(TOP.0, ACCENT.0, position, 500),
            mix(TOP.1, ACCENT.1, position, 500),
            mix(TOP.2, ACCENT.2, position, 500),
        )
    } else {
        (
            mix(ACCENT.0, BOTTOM.0, position - 500, 500),
            mix(ACCENT.1, BOTTOM.1, position - 500, 500),
            mix(ACCENT.2, BOTTOM.2, position - 500, 500),
        )
    }
}

/** paints art with the row gradient, dimming each character by how much ink
it stands for, so the ramp reads as shading rather than a flat silhouette. */
fn render_gradient(art: &str, color: bool) -> String {
    let art = art.trim_matches('\n');
    if !color {
        return art.to_string();
    }

    /// how solid a ramp character is, as a percentage of the row's colour.
    fn density(character: char) -> Option<u16> {
        match character {
            '.' => Some(45),
            ':' | '-' => Some(60),
            '=' | '+' => Some(78),
            '*' | '#' => Some(92),
            '%' | '@' => Some(100),
            _ => None,
        }
    }

    let lines: Vec<&str> = art.lines().collect();
    let rows = lines.len();
    let mut out = String::with_capacity(art.len() * 3);
    let mut painted: Option<(u8, u8, u8)> = None;

    for (row, line) in lines.iter().enumerate() {
        let (r, g, b) = row_color(row, rows);
        for character in line.chars() {
            // one escape per run of same-coloured characters, not per character
            let want = density(character).map(|percent| {
                let scale = |value: u8| (value as u16 * percent / 100) as u8;
                (scale(r), scale(g), scale(b))
            });
            if want != painted {
                match want {
                    Some(rgb) => out.push_str(&fg(rgb)),
                    None => out.push_str(RESET),
                }
                painted = want;
            }
            out.push(character);
        }
        if painted.is_some() {
            out.push_str(RESET);
            painted = None;
        }
        out.push('\n');
    }
    out.pop(); // the art carries no trailing newline
    out
}

pub const LOGO: &str = r#"
           -*=
          *@@-
         #@@@*
        :@@@@@#-
         %@@@@@@#=
         -@@@@@@@@%+
     -*:  +@@@@@@@@@#
   =#@@:  #@@@@*@@@@@*
 :#@@@@@%@@@@@+ *@@@@%
 #@@@@@@@@@@%=  :@@@@* =*
-@@@@@@@@@#=    =@@@@*#@@-
:%@@@@@@@#     +@@@@@@@@%:
-:=*@@@@@#   -%@@@@@@@*=:-
@%*=:-*%@@=  =@@@@%*-:=*%@
@@@@@#=:-+%#- =*+-:=#@@@@@
@@@@@@@@#+::+= :+#@@@@@@@@
%@@@@@@@@@@%::%@@@@@@@@@@%
:*%@@@@@@@@@--@@@@@@@@@%*:
   -*%@@@@@@--@@@@@@%*-
      -+%@@@--@@@%+-
         :+#--#+:
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gradient_runs_pale_through_the_accent_to_deep() {
        let rows = 21;
        let top = row_color(0, rows);
        let middle = row_color(rows / 2, rows);
        let bottom = row_color(rows - 1, rows);

        assert_eq!(top, (0xFF, 0xC2, 0x4A));
        assert_eq!(bottom, (0x8C, 0x16, 0x07));
        // the middle is the brand colour itself, near enough to see it
        let (r, g, b) = middle;
        assert!(
            r.abs_diff(ACCENT.0) < 12 && g.abs_diff(ACCENT.1) < 12 && b.abs_diff(ACCENT.2) < 12,
            "middle row {middle:?} should land on the accent {ACCENT:?}"
        );

        // and it only ever moves one way down the logo
        let reds: Vec<u8> = (0..rows).map(|row| row_color(row, rows).0).collect();
        assert!(
            reds.windows(2).all(|pair| pair[0] >= pair[1]),
            "red channel should fall monotonically: {reds:?}"
        );
    }

    #[test]
    fn a_single_row_and_an_empty_ramp_do_not_divide_by_zero() {
        assert_eq!(row_color(0, 1), (0xFF, 0xC2, 0x4A));
        // rows past the end clamp rather than running off the ramp
        assert_eq!(row_color(99, 4), row_color(3, 4));
    }

    #[test]
    fn without_color_the_art_comes_through_untouched() {
        let plain = logo(false);
        assert_eq!(plain, LOGO.trim_matches('\n'));
        assert!(!plain.contains('\x1b'));
    }

    #[test]
    fn the_logo_fits_the_column_it_is_laid_out_in() {
        let plain = logo(false);
        let lines: Vec<&str> = plain.lines().collect();

        assert!(!lines.is_empty());
        // the layout reserves this much for the logo, see main::print_root_help
        assert!(
            lines.iter().all(|line| line.chars().count() <= 26),
            "the art must stay within 26 columns"
        );
        // no trailing blanks: they would show up as stray padding in the layout
        assert!(lines.iter().all(|line| !line.ends_with(' ')));
        assert!(
            lines
                .iter()
                .flat_map(|line| line.chars())
                .all(|c| " .:-=+*#%@".contains(c)),
            "the art may only use the density ramp"
        );
    }

    #[test]
    fn painting_wraps_every_drawn_character_and_resets_each_row() {
        let painted = logo(true);
        // the leading indent stays bare, the colour opens at the first ink
        let first_line = painted.lines().next().expect("the art has rows");
        let escape = first_line.find('\x1b').expect("the row is painted");
        let ink = first_line
            .find(|c| ".:-=+*#%@".contains(c))
            .expect("the row draws something");
        assert!(escape < ink, "colour opens before the first ink");
        for line in painted.lines() {
            assert!(
                line.ends_with(RESET),
                "each row closes its colour: {line:?}"
            );
        }
        // spaces stay unpainted, so the terminal's own background shows through
        assert_eq!(painted.lines().count(), logo(false).lines().count());
    }
}
