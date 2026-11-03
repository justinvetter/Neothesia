use wgpu_jumpstart::Color;

use crate::{
    render::{QuadInstance, QuadRenderer, TextRenderer, text::TextArea},
    utils::{Point, Size},
};

/// Diatonic step (C=0 .. B=6) within an octave and whether the pitch needs a sharp sign
const STEPS: [(u8, bool); 12] = [
    (0, false),
    (0, true),
    (1, false),
    (1, true),
    (2, false),
    (3, false),
    (3, true),
    (4, false),
    (4, true),
    (5, false),
    (5, true),
    (6, false),
];

/// Diatonic position of E4, the bottom line of the treble staff
const TREBLE_BOTTOM: i32 = 30;
/// Diatonic position of G2, the bottom line of the bass staff
const BASS_BOTTOM: i32 = 18;
/// Diatonic position of middle C
const MIDDLE_C: i32 = 28;

/// Highest and lowest diatonic positions with a line or ledger line we make room for
const TOP_LINE: i32 = 40;
const BOTTOM_LINE: i32 = 16;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StaffNote {
    pub midi: u8,
    /// Linear RGBA, as taken by [`QuadInstance`]
    pub color: [f32; 4],
}

struct Placed {
    diatonic: i32,
    sharp: bool,
    color: [f32; 4],
    /// Notehead is nudged to the right, so it does not overlap a neighbouring second
    shifted: bool,
}

/// Grand staff (treble + bass clef) that shows currently sounding notes
pub struct StaffRenderer {
    pos: Point<f32>,
    size: Size<f32>,

    notes: Vec<StaffNote>,
    /// Glyphs are static for a given layout, so they are shaped once and reused every frame
    text_cache: Vec<TextArea>,
}

fn text_area(
    text: &str,
    family: glyphon::Family,
    font_size: f32,
    x: f32,
    y: f32,
    color: glyphon::Color,
) -> TextArea {
    let font_system = crate::font_system::font_system();
    let font_system = &mut font_system.borrow_mut();

    let mut buffer = glyphon::Buffer::new(font_system, glyphon::Metrics::new(font_size, font_size));
    buffer.set_size(Some(font_size * 2.0), Some(font_size * 2.0));
    buffer.set_wrap(glyphon::Wrap::None);
    buffer.set_text(
        text,
        &glyphon::Attrs::new().family(family),
        glyphon::Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(font_system, false);

    TextArea {
        buffer,
        left: x,
        top: y,
        scale: 1.0,
        bounds: glyphon::TextBounds::default(),
        default_color: color,
    }
}

impl StaffRenderer {
    pub fn new() -> Self {
        Self {
            pos: Default::default(),
            size: Size::new(0.0, 0.0),
            notes: Vec::new(),
            text_cache: Vec::new(),
        }
    }

    pub fn set_rect(&mut self, pos: Point<f32>, size: Size<f32>) {
        if self.pos != pos || self.size != size {
            self.pos = pos;
            self.size = size;
            self.text_cache.clear();
        }
    }

    pub fn set_notes(&mut self, notes: &[StaffNote]) {
        if self.notes != notes {
            self.notes = notes.to_vec();
            self.notes.sort_by_key(|n| n.midi);
            self.text_cache.clear();
        }
    }

    /// Distance between two neighbouring staff lines
    fn space(&self) -> f32 {
        self.size.height / 14.0
    }

    /// Y coordinate of the line (or the centre of a space) at the given diatonic position
    fn y_of(&self, diatonic: i32) -> f32 {
        let s = self.space();
        self.pos.y + s + (TOP_LINE - diatonic) as f32 * s / 2.0
    }

    fn place(&self) -> Vec<Placed> {
        let mut placed: Vec<Placed> = Vec::with_capacity(self.notes.len());

        for note in &self.notes {
            let (step, sharp) = STEPS[(note.midi % 12) as usize];
            let octave = (note.midi / 12) as i32 - 1;
            let diatonic = octave * 7 + step as i32;

            let shifted = placed
                .last()
                .is_some_and(|prev| diatonic - prev.diatonic <= 1 && !prev.shifted);

            placed.push(Placed {
                diatonic,
                sharp,
                color: note.color,
                shifted,
            });
        }

        placed
    }

    fn rebuild_text_cache(&mut self) {
        let s = self.space();
        let ink = glyphon::Color::rgb(20, 20, 25);

        let x = self.pos.x + s * 0.8;
        let music = glyphon::Family::Name("Noto Music");

        // The clef glyphs are positioned relative to the line they wrap around:
        // G clef curls around the G line, F clef sits on the F line
        let treble_line_y = self.y_of(TREBLE_BOTTOM + 2);
        let bass_line_y = self.y_of(BASS_BOTTOM + 6);
        let clef_size = s * 4.6;
        self.text_cache.push(text_area(
            "\u{1D11E}",
            music,
            clef_size,
            x,
            treble_line_y - clef_size * 0.62,
            ink,
        ));
        self.text_cache.push(text_area(
            "\u{1D122}",
            music,
            clef_size,
            x,
            bass_line_y - clef_size * 0.5,
            ink,
        ));

        let note_x = self.note_x();
        let head_w = s * 1.3;
        for p in self.place().iter().filter(|p| p.sharp) {
            let size = s * 2.2;
            let shift = if p.shifted { head_w * 0.95 } else { 0.0 };
            self.text_cache.push(text_area(
                "\u{266F}",
                glyphon::Family::SansSerif,
                size,
                note_x + shift - head_w * 0.5 - size * 0.5,
                self.y_of(p.diatonic) - size * 0.62,
                ink,
            ));
        }
    }

    /// Horizontal centre of the notehead column
    fn note_x(&self) -> f32 {
        self.pos.x + self.size.width * 0.55
    }

    pub fn update(&mut self, quads: &mut QuadRenderer, text: &mut TextRenderer) {
        if self.size.width <= 0.0 || self.size.height <= 0.0 {
            return;
        }

        if self.text_cache.is_empty() {
            self.rebuild_text_cache();
        }

        let s = self.space();
        let line_h = (s * 0.09).max(1.0);
        let left = self.pos.x;
        let width = self.size.width;
        let ink = [0.08, 0.08, 0.1, 1.0];

        // Paper
        quads.push(QuadInstance {
            position: self.pos.into(),
            size: self.size.into(),
            color: Color::new(0.97, 0.96, 0.92, 0.96).into_linear_rgba(),
            border_radius: [s * 0.5; 4],
            ..Default::default()
        });

        let line = |quads: &mut QuadRenderer, y: f32, x: f32, w: f32| {
            quads.push(QuadInstance {
                position: [x, y - line_h / 2.0],
                size: [w, line_h],
                color: Color::new(ink[0], ink[1], ink[2], 1.0).into_linear_rgba(),
                ..Default::default()
            });
        };

        let margin = s * 0.5;
        for bottom in [TREBLE_BOTTOM, BASS_BOTTOM] {
            for i in 0..5 {
                line(quads, self.y_of(bottom + i * 2), left + margin, width - margin * 2.0);
            }
        }

        let note_x = self.note_x();
        let head_w = s * 1.3;
        let head_h = s * 0.95;
        let ledger_w = head_w * 1.9;

        for p in self.place() {
            let d = p.diatonic;
            let cx = note_x + if p.shifted { head_w * 0.95 } else { 0.0 };

            // Ledger lines
            let ledgers: Vec<i32> = if d == MIDDLE_C {
                vec![MIDDLE_C]
            } else if d > TOP_LINE - 1 {
                (TOP_LINE..=d).step_by(2).collect()
            } else if d < BOTTOM_LINE + 1 {
                (d..=BOTTOM_LINE).filter(|l| l % 2 == 0).collect()
            } else {
                Vec::new()
            };
            for l in ledgers {
                line(quads, self.y_of(l), cx - ledger_w / 2.0, ledger_w);
            }

            quads.push(QuadInstance {
                position: [cx - head_w / 2.0, self.y_of(d) - head_h / 2.0],
                size: [head_w, head_h],
                color: p.color,
                border_radius: [head_h / 2.0; 4],
                border_color: [0.08, 0.08, 0.1, 1.0],
                border_width: (s * 0.08).max(1.0),
            });
        }

        text.queue_mut().extend_from_slice(&self.text_cache);
    }
}
