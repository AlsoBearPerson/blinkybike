//! An entirely overengineered utility for colors in embedded rust.
//!
//! This library is entirely motivated by a strange quirk of vscode,
//! where if it sees a sequence like #abcdef anywhere in code,
//! it will render a little color square and color picker UI.
//! (You may have to add a space in front for it to work.)
//!
//! This is of course not quite sane rust syntax, but fine for macros.
//! Amusingly, a color is one token either way: #F5A9B8 is punctuation `#`
//! then the identifier F5A9B8, while #5BCEFA is punctuation `#`
//! then the integer constant 5 with suffix BCEFA.
//!
//! So this crate has some macros that take such CSS-style color strings,
//! and transform them into equivalent RGB8 values at compile time.
//!
//! Additionally, it can perform srgb-to-linear mapping (sometimes called
//! gamma correction). This ensures that colors picked for computer displays
//! look decent on LED strips, assuming the former is tuned for sRGB,
//! and the latter just drives proportional PWM into LEDs.
//!
//! The neat thing about doing this in a proc_macro is that we have access
//! to std and all the floating point powers of the build machine, while
//! the dinky microcontroller driving LEDs may be a bit less mighty.

extern crate proc_macro;
use proc_macro::TokenStream;
use quote::quote;

fn parse_colors(item: TokenStream) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut it = item.into_iter();
    let mut r = Vec::new();
    let mut g = Vec::new();
    let mut b = Vec::new();

    loop {
        match it.next() {
            None => break,
            Some(proc_macro::TokenTree::Punct(p)) if p.as_char() == '#' => (),
            // TODO: Support rgb() and hsl() CSS notation
            _ => panic!("Expected # to start a color.")
        };
        let s = match it.next() {
            Some(proc_macro::TokenTree::Ident(ident)) => ident.to_string(),
            Some(proc_macro::TokenTree::Literal(literal)) => literal.to_string(),
            _ => panic!("Expected color data.")
        };
        if s.len() != 6 {
            panic!("Colors should be exactly 6 characters.");
        }
        r.push(u8::from_str_radix(&s[0..2], 16).unwrap());
        g.push(u8::from_str_radix(&s[2..4], 16).unwrap());
        b.push(u8::from_str_radix(&s[4..6], 16).unwrap());

        match it.next() {
            Some(proc_macro::TokenTree::Punct(punct)) if punct.as_char() == ',' => (),
            None => break,
            _ => panic!("Expected comma.")
        };
    }

    return (r, g, b);
}

/// Converts CSS-style color to equivalent RGB8, without color correction.
///
/// If you intend to send the result into basic linear-PWM hardware,
/// you probably want colors_linear instead.
///
/// Takes a comma-separated sequence of colors of the form #123456,
/// i.e. a `#` character and six hex digits.
#[proc_macro]
pub fn colors_raw(item: TokenStream) -> TokenStream {
    let (r, g, b) = parse_colors(item);

    quote!{ [#( smart_leds::RGB8 { r: #r, g: #g, b: #b } ),*] }.into()
}

fn gamma_adjust(v: u8) -> u8 {
    return (linear_srgb::default::srgb_u8_to_linear(v) * 255.0 + 0.5) as u8;
}

/// Converts CSS-style color to equivalent RGB8, with color correction.
///
/// This should produce better output when sent to linear PWMs,
/// though it does mean that many input values map to the same output,
/// since we're only working with 8 bits per color on both sides.
///
/// Takes a comma-separated sequence of colors of the form #123456,
/// i.e. a `#` character and six hex digits.
#[proc_macro]
pub fn colors_linear(item: TokenStream) -> TokenStream {
    let (mut r, mut g, mut b) = parse_colors(item);
    for v in &mut r { *v = gamma_adjust(*v); }
    for v in &mut g { *v = gamma_adjust(*v); }
    for v in &mut b { *v = gamma_adjust(*v); }

    quote!{ [#( smart_leds::RGB8 { r: #r, g: #g, b: #b } ),*] }.into()
}

/// Outputs an array literal of 256 u8 values,
/// suitable for mapping from an sRGB color value to a linear value.
///
/// Use to embed a `const` or `static` table in your binary,
/// when you need to do runtime gamma correction.
#[proc_macro]
pub fn srgb_to_linear_table(item: TokenStream) -> TokenStream {
    if !item.is_empty() {
        panic!("This macro takes no arguments");
    }
    let v: Vec<_> = (0u8..=255).map(|v| gamma_adjust(v)).collect();

    quote!{ [#(#v),*] }.into()
}
