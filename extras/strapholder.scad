// A 3D printable for a basic holder to keep the LED strip straight,
// keeping it (mostly) from lying sideways, since that's what it tends
// to do coming out of a sideways twist to go around a bend.
//
// Really just a partial circle the size of the corresponding frame tube,
// with a little triangular gripper on front.
// Not designed to grip with any sort of friction, you'll probably want
// zip ties or such to hold things in place,
// and a few of these holders to keep them from folding.
//
// Frame tube diameter, for my particular bike:
// upper - 32.25mm
// rear - 32.25mm
// bottom - 35.5mm
// front - 38.25mm

diam = 38.25;
// Where the grip polygon goes: A smidge past the main circle,
// for a bit of extra stability.
offset = diam / 2 + 1.5;

// Polygon shape for our "gripper" part:
// a U shaped slot of 10mm wide and 12.5mm deep holds the strip,
// with walls that start 1mm wide and slope outwards at a roughly 2:1 ratio,
// eventually meeting up with the main structure circle.
p = [
  [   0,   5],
  [12.5,   5],
  [12.5,   6],
  [-9.0, 14.5],
  [-9.0,-14.5],
  [12.5,  -6],
  [12.5,  -5],
  [   0,  -5],
  [   0,   5],
];

module clamp() {
    translate([offset, 0])
        polygon(p);
    // To keep the gripper front from being pointy, add small circles.
    // Though considering we're working at 1mm width here,
    // printer filament physics probably would already round these for us.
    translate([offset + 12.5, 5.5])
        circle(d=1);
    translate([offset + 12.5, -5.5])
        circle(d=1);
}

module 2dstuff() {
difference() {
    // Our material: Circular to grab the frame, and the clamp.
    union() {
        // 1mm circle, for now. Will see how that holds up.
        circle(d=diam + 2);
        clamp();
    }
    // Our keep-out: Leave room for the actual frame,
    // then cut out on the opposite side of the clamp,
    // leaving enough circle to weakly grip the frame,
    // without forcing too much of a bend to clamp on.
    union() {
        circle(d=diam);
        polygon([[0, 0], [-30, 60], [-30, -60], [0, 0]]);
    }
}
}

// Extrude all that 10mm wide. Chamfers left as an exercise to the reader.
linear_extrude(10) {
    2dstuff();
}
