// Every tool used once, for the screenshots. Run by telamon-screenshot-editor-test --scenario.
(function (win) {
    const d = win.docRef;
    const s = win.backendRef.imageSize;
    const w = s.width, h = s.height;
    const red = "#e5484d", blue = "#3e63dd", green = "#30a46c", yellow = "#ffd60a", black = "#000000";
    // Sizes as the stage computes them for the middle setting.
    const u = Math.max(1, Math.max(w, h) / 1600);
    const stroke = 5 * u, text = 30 * u, marker = 38 * u;

    d.addItem({ type: "rect", x: w * 0.30, y: h * 0.215, w: w * 0.60, h: h * 0.13, color: blue, size: stroke });
    d.addItem({ type: "highlighter", pts: [w * 0.31, h * 0.495, w * 0.56, h * 0.495, w * 0.56, h * 0.497], color: yellow, size: 5 * u });
    d.addItem({ type: "redact", x: w * 0.305, y: h * 0.57, w: w * 0.37, h: h * 0.045, color: black, size: stroke });
    d.addItem({ type: "redact", x: w * 0.38, y: h * 0.255, w: w * 0.20, h: h * 0.04, color: black, size: stroke });
    d.addItem({ type: "arrow", x1: w * 0.18, y1: h * 0.80, x2: w * 0.31, y2: h * 0.73, color: red, size: stroke });
    d.addItem({ type: "marker", x: w * 0.29, y: h * 0.27, number: d.nextMarkerNumber(), color: red, size: marker });
    d.addItem({ type: "marker", x: w * 0.29, y: h * 0.40, number: d.nextMarkerNumber(), color: red, size: marker });
    d.addItem({ type: "marker", x: w * 0.29, y: h * 0.52, number: d.nextMarkerNumber(), color: red, size: marker });
    d.addItem({ type: "text", x: w * 0.46, y: h * 0.74, text: "Click here to continue", color: red, size: text });
    const pts = [];
    for (let i = 0; i <= 40; ++i)
        pts.push(w * 0.62 + i * w * 0.0075, h * 0.78 + Math.sin(i / 3) * h * 0.025);
    d.addItem({ type: "pen", pts: pts, color: green, size: stroke });
    win.tool = "arrow";
})
