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
    translate([offset + 12.5, 5.5])
        circle(d=1);
    translate([offset + 12.5, -5.5])
        circle(d=1);
}

module 2dstuff() {
difference() {
    union() {
        circle(d=diam + 2);
        clamp();
    }
    union() {
        circle(d=diam);
        polygon([[0, 0], [-30, 60], [-30, -60], [0, 0]]);
    }
}
}

linear_extrude(10) {
    2dstuff();
}
