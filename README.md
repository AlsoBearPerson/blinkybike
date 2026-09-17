# Blinkybike

This is the source for a small hobby bike-modding project,
adding some microcontroller-driven RGB lighting to a bicycle.

If you're curious about how it's built,
here's some of the under-the-hood bits.

As a general principle,
the software is supposed to focus on readability and maintability,
over golfing performance.
However, it is also a prototype, and I'm still learning the language,
so there will be quirks.

## Not a recipe

The project is provided solely for reference, as an example of
some practical application of rust-embedded shenanigans.
In particular, it is NOT intended as a ready-made recipe for
easy adaptation: it may contain assumptions of particular
hardware quirks, undocumented subtlety,
and backwards-incompatible changes may happen at any time as I feel like it.
I'm just one internet-connected potato making things,
I have no bandwidth available for any kind of support efforts.

## Parts

For reference, here's the parts being used to drive this, as-built.
These were what I picked up randomly and should not be read as endorsements,
better parts may exist and I did not do systematic market analysis,
but in the spirit of reproducability, here's the major parts used:

### A bicycle

If you don't know what this is, I can't help you.
Mine's acoustic (not electric, powered entirely by my own two legs).

### Microcontroller

Using a [Pi Pico 2W](https://www.raspberrypi.com/products/raspberry-pi-pico-2/?variant=pico-2-w).

Not using the wireless bits yet,
plan is to allow remote control with a smartphone UI via BLE, eventually.

### Blinky Bits

I'm using a [LED fake-neon-tube strip](https://www.adafruit.com/product/3869).

Any WS2812-style "neopixel" strip would work,
the nice thing about the fake neon is that it's a pretty omnidirectional glow,
and the diffusion avoids any sharp point lights,
which might be uncomfortable to look at.

BEWARE: This thing takes some specific handling:
it only bends in one axis, which is great for making neon-like signs,
but a problem for us, if we want to fix it to a 3D object.
Internally, it's a flex-PCB LED strip turned sideways,
thus it bends well side-to-side, twists somewhat,
but REALLY dislikes bending up/down,
if you force it you'll likely rip the PCB.
Be generous with bend radiuses, and try to twist before/after bends,
to keep them along the preferred axis.

Note that this one takes 12V power, which is nice to keep amps low,
but means yet more voltages to worry about.

### Level Shifting

This might not be strictly necessary,
but I'm using an [Adafruit Pixel Shifter](https://www.adafruit.com/product/6066).

Note that the strip wants a 12V supply on its power line,
but 5V WS2811-style signals on its data line,
thus we're boosting from the pico's 3.3V logic up to 5V for a good signal.

### Power

I'm using a USB-C powerbank, as it's portable, swappable,
can do many voltages, and may provide basic power diagnostics itself.
I have a spare one I bought on sale which turned out to have a manufacturing
error of some kind, giving it only 1/3rd of advertised capacity.
A fraction of 90Wh is still perfectly fine for driving a little strip for hours,
mine suggests the build is drawing around 3W in typical use.

Nice thing about USB-C is that you can request various voltages,
and powerbanks in particular tend to broadly support many of them,
but check the labeling for what it can do, and test with a multimeter.
You can get various "USB-C PD sink" adapters, for example Adafruit
sells [nice cables](https://www.adafruit.com/product/5450),
we need 12V for this.

DON'T send the 12V directly to the microcontroller!
The RT6154 regulator on the pico board is only rated for up to 5.5V in.

The microcontroller gets its power from plain old USB-A off the same
powerbank, with the caveat that it draws so little power that the bank
loves to cut off the port after a few minutes
unless put into "low-power output" mode,
which needs to be re-enabled every 2 hours.
Maybe I'll finangle up a resistive dropper, at some point.

Just in case, make sure to common ground lines together.

This entire setup could work off some AA batteries, or some custom
LiPo setup if you're feeling spicy, powerbank was just the easiest option.

### Sensing

None of this is being used yet,
but plan is to tie in a BNO085 to have movement feedback into blinkiness.

Also I want to add some sort of basic illumination sensing,
to automatically tamp down brightness in the dark,
for tunnels and nighttime.

## Legality

So, is this actually road legal?
It depends, different jurisdictions will have different opinions.

Different countries, or even different regions within a country,
may well have different ideas.
Some may be more lenient about a little human-powered portable thing
than drivable murder machines,
others will strictly specify permissible lighting of anything with wheels.
And the opinion of the cop in front of you likely overrules anything in the moment.

Anything you build is entirely at your own risk,
and I can make no warranties here.

But we're not here to be a nuisance,
so here's a few general considerations:

* Avoid running things brighter than necessary,
  we're here to be fun blinky, not blind the neighborhood.
* Avoid fast or distracting animations -
  you don't want to hand out migraines,
  blinking in the corner of one's eye is distracting,
  and anyone staring at you isn't looking at the road.
* Avoid potentially ambiguous use of signal colors, for example you might have:
  * white - front of vehicle
  * red - rear of vehicle, braking
  * yellow - hazard, intent-to-turn
  * blue - reserved for emergency vehicles
* Respect the advice of local law enforcement.

When in doubt, this project is meant for artistic purposes only,
for performance and recording on closed areas,
and should be turned off when transiting public roadways.
