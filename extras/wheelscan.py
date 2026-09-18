#!/usr/bin/env -vS uv run
#
# A quick little script to try and build a color wheel in a nicer color space
# than RGB. We use Oklab/Oklch because it came up in web searches.
#
# Beware that when you just do this naively by throwing a hue wheel with
# constant lightness/chroma at Oklch, you're likely going to see RGB values
# wildly out of range, you might see demands for -400% Red.
#
# Unfortunately, anti-light is a bit beyond the current technical capabilities
# for this project, so that doesn't really work. A lot of internet sources seem
# to just clamp out-of-bounds sRGB to 0 and 255, but that'll result in
# significant distortion, defeating the point of using a perceptually uniform
# color space to begin with.
#
# If you just glance over https://bottosson.github.io/posts/gamutclipping/
# and see the grid of triangles, you might be wondering why we're not just
# taking those lovely high-saturation peaks of strong color.
# If we did that, we'd get perceptually even hues, but lightness/chroma would
# wander all over the place, again creating poor uniformity,
# we'd likely end not much better than a basic RGB wheel.
#
# So we take the other suggestion from the post, and trim down chroma
# until we hit something inside sRGB. This should result in a relatively
# even wheel, with saturation dipping a bit sometimes when colors can't
# be accurately represented with our tech.
#
#
# If you have `uv` installed, you can just run this as
# `uv run extras/wheelscan.py` thanks to the magic comment below.
# Otherwise, any sane python venv/user-env with the `colour-science` package
# installed should do, too.
#
# Other versions will probably work, this is just what I ran with.
#
# /// script
# requires-python = ">=3.14"
# dependencies = [
#     "colour-science>=0.4.7",
# ]
# ///
import colour

def oklch_to_srgb(arr):
    return colour.XYZ_to_sRGB(colour.Oklab_to_XYZ(colour.Oklch_to_Oklab(arr)))

def rgb_in_range(arr):
    for v in arr:
        if v < 0 or v > 1:
            return False
    return True

INIT_L = 0.7
INIT_C = 0.15

C_STEP = 0.001

RGBS = []

def find_colour(hue):
    arr = [INIT_L, INIT_C, hue]
    error = 0

    while True:
        srgb = oklch_to_srgb(arr)
        if rgb_in_range(srgb):
            break
        error += C_STEP
        arr[1] -= C_STEP
    r = int(256 * srgb[0])
    g = int(256 * srgb[1])
    b = int(256 * srgb[2])
    rgb = f"#{r:02X}{g:02X}{b:02X}"
    RGBS.append(rgb)
    print(f"Hue {hue:05.1f} {rgb} {srgb} (err {error})")

# We'd like a power-of-2 table, for easier math.
# 16 entries sounds about right, for a 22.5 degree step.
# Unfortunately, python3's range() *insists* on only integers,
# so we go twice as wide twice as far, then divide down.
for hue in range(0, 720, 45):
    find_colour(hue / 2)

# For copy-paste convenience, print the RGB codes as one neat line.
print(f"RGBs: {', '.join(RGBS)}")
