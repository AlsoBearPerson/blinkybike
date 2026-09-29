// LED animation framework.
//
// A few traits and utilities that make it easier to write a bunch of animations.
//
// A general note: We make heavy use of I8F24 numbers,
// often carrying a number from [0..1) by convention.
// This wastes some bits, but we're a 32bit CPU anyway,
// and it makes some tricks easier.
// But we might consider tightening it to I4F12, perhaps,
// and having code expand that when needed.

#[allow(unused)]
use defmt::*;
use fixed::types::*;
use smart_leds::RGB8;

// General API for LED animations:
// Given a time value that strobes through [0..1),
// fill the leds with colors to render.
// Most of the time, an animation will see exactly one ramp from 0..1,
// before being swapped out.
pub trait Animation {
    // State should be used for large, temporary state,
    // such as pixel buffers.
    // Smaller, non-reproducible state (like fixed color mappings)
    // may be stored on the implementing object directly.
    // State will be dropped whenever the animation switches,
    // and reinitialized to a default() value as an animation becomes active.
    type State: Default;
    fn frame(&self, state: &mut Self::State, time: I8F24, leds: &mut [RGB8]);
}

// A simple animation made up of reusable component pieces.
// See the traits below for a description of each piece.
struct StatelessAnimation<LM, PM, CL>
where
    LM: LedMapper,
    PM: PixelMapper,
    CL: ColorLookup,
{
    lm: LM,
    pm: PM,
    cl: CL,
}
// Map each LED index into an abstract offset in [0..1).
// Leaving this extensible allows flexible layouts for complex arrangements,
// where some patterns want to treat all LEDs as one continuous sequence,
// while others might prefer to mirror between a top/bottom strip.
pub trait LedMapper {
    fn remap(&self, led: usize) -> I8F24;
}
// Map time and LED offset into an overall pattern offset.
// This allows us to have a classic infinite scroll,
// but also a bounce, or other more subtle effects,
// retaining access to swappable color patterns.
trait PixelMapper {
    // This function is allowed to return values outside [0, 1],
    // which get mapped to an unlit LED without consulting ColorLookup.
    fn remap(&self, time: I8F24, led: I8F24) -> I8F24;
}
// Map an abstract pattern offset from [0..1] into a concrete color.
// Typically, scanning the pattern offset will paint a base pattern,
// which the mappers then riff on.
// Having 0 and 1 return the same color is usually helpful, but not required.
trait ColorLookup {
    fn to_color(&self, offset: I8F24) -> RGB8;
}
pub trait CanAdvance {
    fn advance(&mut self);
}

impl<LM, PM, CL> StatelessAnimation<LM, PM, CL>
where
    LM: LedMapper,
    PM: PixelMapper,
    CL: ColorLookup,
{
    fn get(&self, time: I8F24, led: usize) -> RGB8 {
        let led = self.lm.remap(led);
        let offset = self.pm.remap(time, led);
        if offset < 0 || offset > 1 {
            return RGB8::default();
        }
        return self.cl.to_color(offset);
    }
}

impl<LM, PM, CL> Animation for StatelessAnimation<LM, PM, CL>
where
    LM: LedMapper,
    PM: PixelMapper,
    CL: ColorLookup,
{
    type State = ();

    fn frame(&self, _state: &mut Self::State, time: I8F24, leds: &mut [RGB8]) {
        for (i, led) in leds.iter_mut().enumerate() {
            *led = self.get(time, i);
        }
    }
}

impl<LM, PM, CL> CanAdvance for StatelessAnimation<LM, PM, CL>
where
    LM: LedMapper,
    PM: PixelMapper,
    CL: ColorLookup,
    CL: CanAdvance,
{
    fn advance(&mut self) {
        self.cl.advance();
    }
}

// Basic linear mapping, assuming a fixed LED layout.
// Will exceed intended range if the number of LEDs requested differs.
#[allow(unused)]
pub struct Linear<const NUM_LEDS: usize>;
impl<const NUM_LEDS: usize> LedMapper for Linear<NUM_LEDS> {
    fn remap(&self, led: usize) -> I8F24 {
        let frac: I8F24 = I8F24::ONE / (NUM_LEDS as i32);
        return frac * (led as i32);
    }
}

// Linear two-strip mapping, flipping the second strip.
// Suited for setups where two strips are laid out with their inputs together,
// then wrap around a center and meet up end-to-end on the other side,
// and so reversing one of them means our mapped offset walks the circle.
pub struct LinRev<const N1_LEDS: usize, const N2_LEDS: usize>;
impl<const N1_LEDS: usize, const N2_LEDS: usize> LedMapper for LinRev<N1_LEDS, N2_LEDS> {
    fn remap(&self, led: usize) -> I8F24 {
        let frac: I8F24 = I8F24::ONE / (N1_LEDS + N2_LEDS) as i32;
        let led = if led < N1_LEDS {
            led
        } else {
            // Reverse order of second strip: strip2 0 is last,
            // strip 2 N-1 is first
            let rev_index = N1_LEDS + N2_LEDS - 1 - led;
            N1_LEDS + rev_index
        };
        return frac * (led as i32);
    }
}

// Basic infinite repeating slide, sliding away from LED offset 0,
// as we go along the strip we walk "into the past",
// as time passes values walk down the strip away from 0.
struct Slide;
impl PixelMapper for Slide {
    fn remap(&self, time: I8F24, led: I8F24) -> I8F24 {
        let result: I8F24 = time - led;
        if result < 0 {
            return result + I8F24::ONE;
        } else {
            return result;
        }
    }
}

// Similar to Slide, but leaves a gap of unlit leds between repeats,
// for improved presentation of flags and the like.
// The unlit area starts at the front at led offset 0,
// then walks backwards like slide.
struct SlideGap;
impl PixelMapper for SlideGap {
    fn remap(&self, time: I8F24, led: I8F24) -> I8F24 {
        const GAP_WIDTH: I8F24 = I8F24::lit("0.25");
        return Slide.remap(time, led) * (I8F24::ONE + GAP_WIDTH) - GAP_WIDTH;
    }
}

// A basic RGB wheel, driving 255 total across channels at all times,
// and keeping one channel at 0 at a time, ramping through the others.
struct RampWheel;
impl ColorLookup for RampWheel {
    fn to_color(&self, offset: I8F24) -> RGB8 {
        // This multiplies by 256, basically.
        let offset: I16F16 = I16F16::from_bits(offset.to_bits());
        // We ramp in 3 sections: R->G, G->B, B->R.
        let offset: I16F16 = offset * 3;
        // offset is now in [0 .. 768) for our three sections
        const C256: I16F16 = I16F16::lit("256");
        if offset < C256 {
            let trunc: u8 = offset.to_num();
            return RGB8 {
                r: 255 - trunc,
                g: trunc,
                b: 0,
            };
        }
        let offset: I16F16 = offset - C256;
        if offset < C256 {
            let trunc: u8 = offset.to_num();
            return RGB8 {
                r: 0,
                g: 255 - trunc,
                b: trunc,
            };
        }
        let offset = offset - C256;
        let trunc: u8 = offset.to_num();
        return RGB8 {
            r: trunc,
            g: 0,
            b: 255 - trunc,
        };
    }
}

// A truncating lookup, mapping segments of the offset space
// to colors from a slice, each color getting an equal sized segment.
struct FlagLookup(&'static [RGB8]);
impl ColorLookup for FlagLookup {
    fn to_color(&self, offset: I8F24) -> RGB8 {
        let offset: I8F24 = offset * (self.0.len() as i32);
        let offset: usize = offset.to_num();
        if offset < self.0.len() {
            return self.0[offset];
        } else {
            return *self.0.last().unwrap();
        }
    }
}

struct ManyFlags {
    flags: &'static [&'static [RGB8]],
    cur_index: usize,
}
impl ManyFlags {
    fn new(flags: &'static [&'static [smart_leds::RGB<u8>]]) -> Self {
        Self {
            flags,
            cur_index: 0,
        }
    }
}
impl ColorLookup for ManyFlags {
    fn to_color(&self, offset: I8F24) -> RGB8 {
        let flag = self.flags[self.cur_index];
        return FlagLookup(flag).to_color(offset);
    }
}
impl CanAdvance for ManyFlags {
    fn advance(&mut self) {
        self.cur_index = (self.cur_index + 1) % self.flags.len();
    }
}

// An interpolating lookup, where 0 is the first color,
// multiples of 1/colors.len() are corresponding indices,
// and values in between linearly blend adjacent colors.
//
// note that said blend happens in linear RGB space,
// so you might want to add some midpoints to avoid
// poor perceptual blending.
struct SmoothStep(&'static [RGB8]);
impl ColorLookup for SmoothStep {
    fn to_color(&self, offset: I8F24) -> RGB8 {
        let offset: I8F24 = offset * (self.0.len() as i32);
        let index: usize = offset.to_num();
        let (prev, next) = if index < self.0.len() - 1 {
            (self.0[index], self.0[index + 1])
        } else {
            (*self.0.last().unwrap(), self.0[0])
        };
        // We drop to 16-bit for lerping, since our colors are only 8bit anyway,
        // and this might be a fair chunk of double-width math.
        let fractional: U1F15 = offset.frac().saturating_to_num();
        prev.iter()
            .zip(next.iter())
            .map(|(pc, nc)| {
                let pc: U8F8 = pc.into();
                fractional.lerp(pc, nc.into()).to_num()
            })
            .collect()
    }
}

pub fn rgbwheel<LM: LedMapper>(lm: LM) -> impl Animation {
    StatelessAnimation {
        lm,
        pm: Slide,
        cl: RampWheel,
    }
}

pub fn smoothwheel<LM: LedMapper>(lm: LM, colors: &'static [RGB8]) -> impl Animation {
    StatelessAnimation {
        lm,
        pm: Slide,
        cl: SmoothStep(colors),
    }
}

pub fn many_flag<LM: LedMapper>(
    lm: LM,
    flags: &'static [&'static [RGB8]],
) -> impl Animation + CanAdvance {
    StatelessAnimation {
        lm,
        pm: SlideGap,
        cl: ManyFlags::new(flags),
    }
}
