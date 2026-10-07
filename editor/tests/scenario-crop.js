// A crop set, with the crop tool on: handles and the dimmed rest.
(function (win) {
    const s = win.backendRef.imageSize;
    win.docRef.addItem({ type: "arrow", x1: s.width * 0.18, y1: s.height * 0.8, x2: s.width * 0.31, y2: s.height * 0.73, color: "#e5484d", size: 6 });
    win.docRef.setCrop({ x: Math.round(s.width * 0.08), y: Math.round(s.height * 0.12), w: Math.round(s.width * 0.84), h: Math.round(s.height * 0.72) });
    win.tool = "crop";
})
