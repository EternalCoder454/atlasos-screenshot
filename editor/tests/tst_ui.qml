import QtQuick
import QtTest
import net.eterneon.telamon.screenshoteditor

// The editor window, driven with the mouse and the keyboard.
TestCase {
    id: tc
    name: "UI"
    when: windowShown

    Component {
        id: mainComp
        Main {
            startSource: testImagePath
        }
    }

    property var win: null

    function init() {
        win = createTemporaryObject(mainComp, null);
        verify(win);
        tryVerify(() => win.backendRef.imageSize.width === 400, 5000);
        win.requestActivate();
        waitForRendering(win.contentItem);
    }

    function cleanup() {
        if (win) {
            win._forceClose = true;
            win.close();
            win.destroy();
            win = null;
        }
    }

    // Item coordinates of an image pixel.
    function at(x, y) {
        return win.stageRef.canvas.toItem(x, y);
    }

    function drag(x1, y1, x2, y2, steps) {
        const stage = win.stageRef;
        const a = at(x1, y1), b = at(x2, y2);
        const n = steps || 5;
        mousePress(stage, a.x, a.y);
        for (let i = 1; i <= n; ++i)
            mouseMove(stage, a.x + (b.x - a.x) * i / n, a.y + (b.y - a.y) * i / n);
        mouseRelease(stage, b.x, b.y);
    }

    function compareRect(actual, x, y, w, h) {
        verify(actual !== null, "no rectangle");
        // The pointer is in whole item pixels and the picture sits at a half pixel: one off is fine.
        const got = [actual.x, actual.y, actual.w, actual.h], want = [x, y, w, h];
        for (let i = 0; i < 4; ++i)
            verify(Math.abs(got[i] - want[i]) <= 1, "got " + JSON.stringify(got) + ", wanted " + JSON.stringify(want));
    }

    function typeText(text) {
        for (const c of text)
            keyClick(c);
    }

    function click(x, y) {
        const p = at(x, y);
        mouseClick(win.stageRef, p.x, p.y);
    }

    function test_opens_the_image() {
        compare(win.backendRef.imageSize, Qt.size(400, 300));
        verify(win.hasImage);
        compare(win.docRef.items.length, 0);
        verify(!win.docRef.needsSave);
        compare(win.title, "Screenshot");
        // The picture is fitted, not stretched.
        verify(win.stageRef.canvas.scale > 0 && win.stageRef.canvas.scale <= 1);
    }

    function test_arrow() {
        win.tool = "arrow";
        drag(50, 50, 200, 120);
        compare(win.docRef.items.length, 1);
        const a = win.docRef.items[0];
        compare(a.type, "arrow");
        fuzzyCompare(a.x1, 50, 1);
        fuzzyCompare(a.y1, 50, 1);
        fuzzyCompare(a.x2, 200, 1);
        fuzzyCompare(a.y2, 120, 1);
        verify(win.docRef.needsSave);
        verify(win.title.indexOf("[modified]") > 0);
    }

    function test_a_click_without_a_drag_draws_no_arrow_or_box() {
        win.tool = "arrow";
        click(50, 50);
        win.tool = "rect";
        click(60, 60);
        win.tool = "redact";
        click(70, 70);
        compare(win.docRef.items.length, 0);
    }

    function test_rect_and_redact() {
        win.tool = "rect";
        drag(100, 100, 40, 60);   // backwards: normalised
        compare(win.docRef.items.length, 1);
        compare(win.docRef.items[0].type, "rect");
        fuzzyCompare(win.docRef.items[0].x, 40, 1);
        fuzzyCompare(win.docRef.items[0].w, 60, 1.5);
        win.tool = "redact";
        drag(10, 10, 80, 30);
        const r = win.docRef.items[1];
        compare(r.type, "redact");
        compare(r.color, "#000000");   // black by default, opaque
    }

    function test_each_tool_keeps_its_colour() {
        win.tool = "highlighter";
        compare(win.stageRef.drawColor, "#ffd60a");
        win.tool = "arrow";
        compare(win.stageRef.drawColor, "#e5484d");
        win.setColorIndex(4);
        compare(win.stageRef.drawColor, "#3e63dd");
        win.tool = "pen";
        compare(win.stageRef.drawColor, "#e5484d");
        win.tool = "arrow";
        compare(win.stageRef.drawColor, "#3e63dd");
    }

    function test_pen_follows_the_pointer() {
        win.tool = "pen";
        drag(20, 20, 220, 20, 20);
        compare(win.docRef.items.length, 1);
        const pen = win.docRef.items[0];
        compare(pen.type, "pen");
        verify(pen.pts.length >= 6);
        compare(pen.pts.length % 2, 0);
        fuzzyCompare(pen.pts[0], 20, 1);
        fuzzyCompare(pen.pts[pen.pts.length - 2], 220, 1);
    }

    function test_markers_count_up_and_after_undo() {
        win.tool = "marker";
        click(30, 30);
        click(60, 60);
        click(90, 90);
        compare(win.docRef.items.map(m => m.number), [1, 2, 3]);
        win.docRef.undo();
        click(120, 120);
        compare(win.docRef.items.map(m => m.number), [1, 2, 3]);
    }

    function test_text_is_typed_and_placed() {
        win.tool = "text";
        click(30, 200);
        verify(win.stageRef.editing);
        typeText("Hello <b>x</b>");
        keyClick(Qt.Key_Return, Qt.ControlModifier);
        verify(!win.stageRef.editing);
        compare(win.docRef.items.length, 1);
        compare(win.docRef.items[0].type, "text");
        // Plain text: the markup stays as typed.
        compare(win.docRef.items[0].text, "Hello <b>x</b>");
        // Empty text is not an annotation.
        click(30, 250);
        verify(win.stageRef.editing);
        win.stageRef.commitText();
        compare(win.docRef.items.length, 1);
    }

    function test_changing_tool_places_the_text() {
        win.tool = "text";
        click(30, 200);
        typeText("note");
        win.tool = "arrow";
        compare(win.docRef.items.length, 1);
        compare(win.docRef.items[0].text, "note");
    }

    function test_escape_cancels_text_and_gestures_before_it_closes() {
        win.tool = "text";
        click(30, 200);
        typeText("gone");
        keyClick(Qt.Key_Escape);
        verify(!win.stageRef.editing);
        compare(win.docRef.items.length, 0);
        verify(win.visible);

        win.tool = "arrow";
        const a = at(50, 50), b = at(150, 100);
        mousePress(win.stageRef, a.x, a.y);
        mouseMove(win.stageRef, b.x, b.y);
        verify(win.stageRef.gesturing);
        win.stageRef.cancelGesture();
        mouseRelease(win.stageRef, b.x, b.y);
        compare(win.docRef.items.length, 0);
        verify(!win.stageRef.gesturing);
    }

    function test_crop_by_dragging_and_handles() {
        win.tool = "crop";
        drag(50, 40, 250, 190);
        verify(win.docRef.crop !== null);
        compareRect(win.docRef.crop, 50, 40, 200, 150);
        // Resize with the south-east handle.
        const h = at(250, 190);
        mousePress(win.stageRef, h.x, h.y);
        const t = at(300, 240);
        mouseMove(win.stageRef, t.x, t.y);
        mouseRelease(win.stageRef, t.x, t.y);
        compareRect(win.docRef.crop, 50, 40, 250, 200);
        // Move it.
        const inside = at(150, 140);
        mousePress(win.stageRef, inside.x, inside.y);
        const to = at(160, 150);
        mouseMove(win.stageRef, to.x, to.y);
        mouseRelease(win.stageRef, to.x, to.y);
        compareRect(win.docRef.crop, 60, 50, 250, 200);
        // Undo goes through the three steps.
        win.docRef.undo();
        compareRect(win.docRef.crop, 50, 40, 250, 200);
        win.docRef.undo();
        win.docRef.undo();
        compare(win.docRef.crop, null);
        // Leaving the tool shows the cropped picture.
        win.docRef.redo();
        win.tool = "arrow";
        compareRect(win.stageRef.shown, 50, 40, 200, 150);
        // A crop of everything is no crop.
        win.tool = "crop";
        drag(-20, -20, 420, 320);
        compare(win.docRef.crop, null);
    }

    function test_a_shape_cannot_leave_the_picture() {
        win.tool = "rect";
        const a = at(300, 200);
        const far = at(900, 900);
        mousePress(win.stageRef, a.x, a.y);
        mouseMove(win.stageRef, far.x, far.y);
        mouseRelease(win.stageRef, far.x, far.y);
        const r = win.docRef.items[0];
        verify(r.x + r.w <= 400 + 0.01);
        verify(r.y + r.h <= 300 + 0.01);
    }

    function test_save_goes_through_the_cli() {
        win.tool = "redact";
        drag(10, 10, 100, 60);
        win.docRef.setCrop({ x: 0, y: 0, w: 200, h: 150 });
        verify(win.docRef.needsSave);
        const spy = createTemporaryObject(spyComp, tc, { target: win.backendRef, signalName: "helperFinished" });
        win.save();
        spy.wait(5000);
        compare(spy.count, 1);
        compare(spy.signalArguments[0][0], "save");
        verify(spy.signalArguments[0][1], spy.signalArguments[0][2]);
        verify(!win.docRef.dirty);
        // What was saved has the size of the crop.
        const img = createTemporaryObject(imgComp, tc, { source: "file://" + spy.signalArguments[0][2] });
        tryCompare(img, "status", Image.Ready);
        compare(img.sourceSize, Qt.size(200, 150));
    }

    function test_copy_failure_keeps_the_work() {
        win.tool = "arrow";
        drag(50, 50, 200, 120);
        const spy = createTemporaryObject(spyComp, tc, { target: win.backendRef, signalName: "helperFinished" });
        win.backendRef.copy(win.docRef.items, Qt.rect(0, 0, 0, 0));
        spy.wait(5000);
        compare(spy.signalArguments[0][0], "copy");
        verify(spy.signalArguments[0][1]);
        compare(win.docRef.items.length, 1);
        verify(win.visible);
    }

    function test_undo_redo_by_keyboard() {
        win.tool = "arrow";
        drag(50, 50, 200, 120);
        win.stageRef.forceActiveFocus();
        keyClick(Qt.Key_Z, Qt.ControlModifier);
        compare(win.docRef.items.length, 0);
        keyClick(Qt.Key_Z, Qt.ControlModifier | Qt.ShiftModifier);
        compare(win.docRef.items.length, 1);
        keyClick(Qt.Key_Z, Qt.ControlModifier);
        keyClick(Qt.Key_Y, Qt.ControlModifier);
        compare(win.docRef.items.length, 1);
    }

    function test_tool_keys() {
        win.stageRef.forceActiveFocus();
        keyClick(Qt.Key_R);
        compare(win.tool, "rect");
        keyClick(Qt.Key_P);
        compare(win.tool, "pen");
        keyClick(Qt.Key_H);
        compare(win.tool, "highlighter");
        keyClick(Qt.Key_N);
        compare(win.tool, "marker");
        keyClick(Qt.Key_X);
        compare(win.tool, "redact");
        keyClick(Qt.Key_C);
        compare(win.tool, "crop");
        keyClick(Qt.Key_A);
        compare(win.tool, "arrow");
        keyClick(Qt.Key_T);
        compare(win.tool, "text");
        // While typing the letters are text, not tools.
        click(30, 200);
        typeText("rpa");
        compare(win.tool, "text");
        compare(win.stageRef.editing, true);
    }

    function test_close_without_work_just_closes() {
        const closing = createTemporaryObject(spyComp, tc, { target: win, signalName: "closing" });
        win.stageRef.forceActiveFocus();
        keyClick(Qt.Key_Escape);
        compare(closing.count, 1);
    }

    function test_close_with_unsaved_work_asks() {
        win.tool = "arrow";
        drag(50, 50, 200, 120);
        win.stageRef.forceActiveFocus();
        keyClick(Qt.Key_W, Qt.ControlModifier);
        wait(100);
        verify(win.visible);   // still here: the question is open
        verify(win.docRef.needsSave);
    }

    Component {
        id: spyComp
        SignalSpy {}
    }
    Component {
        id: imgComp
        Image {}
    }
}
