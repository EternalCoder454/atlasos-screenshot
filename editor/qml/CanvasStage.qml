pragma ComponentBehavior: Bound

import QtQuick
import Telamon.Ui

// The picture and the drawing on it: pointer gestures for every tool, the
// crop handles and the text box. The drawing itself is CanvasView (C++), and
// everything it stores is in image pixels.
FocusScope {
    id: stage

    required property Document document
    required property Backend backend

    property string tool: "arrow"
    property string drawColor: "#e5484d" // telamon-lint: allow-raw annotation colour, content of the image
    property int sizeIndex: 1

    readonly property size imageSize: backend.imageSize
    readonly property bool hasImage: imageSize.width > 0
    // Sizes are chosen for a 1600 px picture and grow with a larger one, so
    // a 4K screenshot is not marked with hairlines.
    readonly property real unit: Math.max(1, Math.max(imageSize.width, imageSize.height) / 1600)
    readonly property real strokeSize: [3, 5, 8][sizeIndex] * unit
    readonly property real textSize: [20, 30, 44][sizeIndex] * unit
    readonly property real markerSize: [28, 38, 52][sizeIndex] * unit

    // The part of the image on show: all of it while cropping, else the crop.
    readonly property var shown: (tool === "crop" || !document.crop) ? fullRect() : document.crop
    readonly property real viewScale: view.scale
    // For the tests.
    readonly property alias canvas: view

    property var draft: null
    property var cropDraft: null
    property bool editing: false
    property real editX: 0
    property real editY: 0
    property bool _pressed: false
    property bool _cancelled: false
    property var _anchor: null
    property string _cropMode: ""
    property var _cropStart: null
    property var _grab: null
    property string hoverCursorTool: ""

    // Something is half done: Escape cancels it instead of closing.
    readonly property bool gesturing: draft !== null || cropDraft !== null || editing || _pressed

    Accessible.role: Accessible.Canvas
    Accessible.name: qsTr("Screenshot")
    Accessible.description: qsTr("Drawing area. Current tool: %1").arg(stage.toolName)
    property string toolName: ""

    function fullRect() {
        return { x: 0, y: 0, w: stage.imageSize.width, h: stage.imageSize.height };
    }

    function clampPoint(p) {
        const r = stage.shown;
        return {
            x: Math.max(r.x, Math.min(r.x + r.w, p.x)),
            y: Math.max(r.y, Math.min(r.y + r.h, p.y))
        };
    }

    function pointAt(x, y) {
        const p = view.toImage(x, y);
        return stage.clampPoint({ x: p.x, y: p.y });
    }

    // ---- crop ----------------------------------------------------------

    readonly property real minCrop: 8

    function cropRectNow() {
        return stage.cropDraft ?? stage.document.crop ?? stage.fullRect();
    }

    // "nw", "n", ... for a handle under the pointer, "move" inside the
    // rectangle, "" outside.
    function cropHit(x, y) {
        const r = stage.cropRectNow();
        const tol = 11;
        const xs = [r.x, r.x + r.w / 2, r.x + r.w];
        const ys = [r.y, r.y + r.h / 2, r.y + r.h];
        const names = [["nw", "n", "ne"], ["w", "", "e"], ["sw", "s", "se"]];
        for (let j = 0; j < 3; ++j) {
            for (let i = 0; i < 3; ++i) {
                if (!names[j][i])
                    continue;
                const q = view.toItem(xs[i], ys[j]);
                if (Math.abs(q.x - x) <= tol && Math.abs(q.y - y) <= tol)
                    return names[j][i];
            }
        }
        const a = view.toItem(r.x, r.y);
        const b = view.toItem(r.x + r.w, r.y + r.h);
        if (x >= a.x && x <= b.x && y >= a.y && y <= b.y)
            return "move";
        return "";
    }

    function cropCursor(hit) {
        switch (hit) {
        case "nw": case "se": return Qt.SizeFDiagCursor;
        case "ne": case "sw": return Qt.SizeBDiagCursor;
        case "n": case "s": return Qt.SizeVerCursor;
        case "e": case "w": return Qt.SizeHorCursor;
        case "move": return Qt.SizeAllCursor;
        default: return Qt.CrossCursor;
        }
    }

    function resizedCrop(s, id, p) {
        const W = stage.imageSize.width, H = stage.imageSize.height;
        let l = s.x, t = s.y, r = s.x + s.w, b = s.y + s.h;
        if (id.indexOf("w") >= 0) l = Math.min(p.x, r - stage.minCrop);
        if (id.indexOf("e") >= 0) r = Math.max(p.x, l + stage.minCrop);
        if (id.indexOf("n") >= 0) t = Math.min(p.y, b - stage.minCrop);
        if (id.indexOf("s") >= 0) b = Math.max(p.y, t + stage.minCrop);
        l = Math.max(0, Math.round(l));
        t = Math.max(0, Math.round(t));
        r = Math.min(W, Math.round(r));
        b = Math.min(H, Math.round(b));
        return { x: l, y: t, w: Math.max(1, r - l), h: Math.max(1, b - t) };
    }

    function commitCrop(r) {
        const full = r.x <= 0 && r.y <= 0 && r.w >= stage.imageSize.width && r.h >= stage.imageSize.height;
        stage.document.setCrop(full ? null : r);
    }

    // ---- text ----------------------------------------------------------

    function startText(p) {
        stage.editX = p.x;
        stage.editY = p.y;
        editor.text = "";
        stage.editing = true;
        editor.forceActiveFocus();
    }

    function commitText() {
        if (!stage.editing)
            return;
        const t = editor.text;
        stage.editing = false;
        if (t.trim().length > 0) {
            stage.document.addItem({
                type: "text", x: stage.editX, y: stage.editY, text: t,
                color: stage.drawColor, size: stage.textSize
            });
        }
        stage.forceActiveFocus();
    }

    function cancelText() {
        if (!stage.editing)
            return;
        stage.editing = false;
        stage.forceActiveFocus();
    }

    // Puts a text box in progress into the document (before a save or a tool change).
    function finishEditing() {
        stage.commitText();
    }

    function refocus() {
        if (stage.editing)
            editor.forceActiveFocus();
    }

    // Cancels what is half done. True when there was something.
    function cancelGesture() {
        if (stage.editing) {
            stage.cancelText();
            return true;
        }
        if (stage.draft !== null || stage.cropDraft !== null || stage._pressed) {
            stage.draft = null;
            stage.cropDraft = null;
            stage._cancelled = true;
            return true;
        }
        return false;
    }

    onToolChanged: {
        stage.commitText();
        stage.draft = null;
        stage.cropDraft = null;
    }

    // ---- pointer -------------------------------------------------------

    function press(x, y) {
        if (!stage.hasImage)
            return;
        stage.forceActiveFocus();
        stage._pressed = true;
        stage._cancelled = false;
        const p = stage.pointAt(x, y);
        const color = stage.drawColor;
        switch (stage.tool) {
        case "arrow":
            stage.draft = { type: "arrow", x1: p.x, y1: p.y, x2: p.x, y2: p.y, color: color, size: stage.strokeSize };
            break;
        case "rect":
        case "redact":
            stage._anchor = p;
            stage.draft = {
                type: stage.tool, x: p.x, y: p.y, w: 0, h: 0, size: stage.strokeSize,
                color: stage.tool === "redact" ? stage.drawColor : color
            };
            break;
        case "pen":
        case "highlighter":
            stage.draft = { type: stage.tool, pts: [p.x, p.y], color: color, size: stage.strokeSize };
            break;
        case "marker":
            stage.document.addItem({
                type: "marker", x: p.x, y: p.y, number: stage.document.nextMarkerNumber(),
                color: color, size: stage.markerSize
            });
            break;
        case "text":
            stage.commitText();
            stage.startText(p);
            break;
        case "crop": {
            let hit = stage.cropHit(x, y);
            // With nothing cropped yet the "rectangle" is the whole picture:
            // a drag inside it draws a new one.
            if (hit === "move" && !stage.document.crop)
                hit = "";
            const cur = stage.cropRectNow();
            stage._cropStart = { x: cur.x, y: cur.y, w: cur.w, h: cur.h };
            if (hit === "") {
                stage._cropMode = "new";
                stage._anchor = p;
            } else {
                stage._cropMode = hit;
                stage._grab = p;
            }
            stage.cropDraft = stage._cropStart;
            break;
        }
        }
    }

    function move(x, y) {
        if (!stage._pressed) {
            if (stage.tool === "crop")
                stage.hoverCursorTool = "crop:" + stage.cropHit(x, y);
            return;
        }
        if (stage._cancelled || !stage.hasImage)
            return;
        const p = stage.pointAt(x, y);
        const d = stage.draft;
        switch (stage.tool) {
        case "arrow":
            if (d) {
                d.x2 = p.x;
                d.y2 = p.y;
                stage.draftChanged();
            }
            break;
        case "rect":
        case "redact":
            if (d) {
                const a = stage._anchor;
                d.x = Math.min(a.x, p.x);
                d.y = Math.min(a.y, p.y);
                d.w = Math.abs(p.x - a.x);
                d.h = Math.abs(p.y - a.y);
                stage.draftChanged();
            }
            break;
        case "pen":
        case "highlighter":
            if (d) {
                const n = d.pts.length;
                const dx = p.x - d.pts[n - 2], dy = p.y - d.pts[n - 1];
                const min = 1.5 / Math.max(0.0001, view.scale);
                if (dx * dx + dy * dy >= min * min) {
                    d.pts.push(p.x, p.y);
                    stage.draftChanged();
                }
            }
            break;
        case "crop": {
            const s = stage._cropStart;
            if (stage._cropMode === "new") {
                const a = stage._anchor;
                const l = Math.round(Math.min(a.x, p.x)), t = Math.round(Math.min(a.y, p.y));
                const r = Math.round(Math.max(a.x, p.x)), b = Math.round(Math.max(a.y, p.y));
                stage.cropDraft = { x: l, y: t, w: Math.max(1, r - l), h: Math.max(1, b - t) };
            } else if (stage._cropMode === "move") {
                const W = stage.imageSize.width, H = stage.imageSize.height;
                const nx = Math.round(Math.max(0, Math.min(W - s.w, s.x + p.x - stage._grab.x)));
                const ny = Math.round(Math.max(0, Math.min(H - s.h, s.y + p.y - stage._grab.y)));
                stage.cropDraft = { x: nx, y: ny, w: s.w, h: s.h };
            } else {
                stage.cropDraft = stage.resizedCrop(s, stage._cropMode, p);
            }
            break;
        }
        }
    }

    function release(x, y) {
        if (!stage._pressed)
            return;
        stage._pressed = false;
        if (stage._cancelled) {
            stage._cancelled = false;
            return;
        }
        const d = stage.draft;
        const minLen = 3 / Math.max(0.0001, view.scale);
        switch (stage.tool) {
        case "arrow":
            if (d && Math.hypot(d.x2 - d.x1, d.y2 - d.y1) >= minLen)
                stage.document.addItem(d);
            break;
        case "rect":
        case "redact":
            if (d && d.w >= minLen && d.h >= minLen)
                stage.document.addItem(d);
            break;
        case "pen":
        case "highlighter":
            if (d)
                stage.document.addItem(d);
            break;
        case "crop":
            if (stage.cropDraft && stage._cropMode !== "") {
                const r = stage.cropDraft;
                if (r.w >= stage.minCrop && r.h >= stage.minCrop)
                    stage.commitCrop(r);
            }
            stage._cropMode = "";
            stage.cropDraft = null;
            break;
        }
        stage.draft = null;
    }

    // ---- view ----------------------------------------------------------

    CanvasView {
        id: view
        anchors.fill: parent
        image: stage.backend.image
        items: stage.document.items
        draft: stage.draft ?? ({})
        viewRect: Qt.rect(stage.shown.x, stage.shown.y, stage.shown.w, stage.shown.h)
        maxScale: 1 / Screen.devicePixelRatio
        // While cropping, the image outside the crop is dimmed.
        clearRect: stage.tool === "crop" ? Qt.rect(stage.cropRectNow().x, stage.cropRectNow().y, stage.cropRectNow().w, stage.cropRectNow().h) : Qt.rect(0, 0, 0, 0)
    }

    // The crop: the image outside it is dimmed, with handles on its edge.
    Item {
        id: cropOverlay
        anchors.fill: parent
        visible: stage.tool === "crop" && stage.hasImage

        readonly property var r: stage.cropRectNow()
        readonly property real cLeft: view.offset.x + r.x * view.scale
        readonly property real cTop: view.offset.y + r.y * view.scale
        readonly property real cRight: cLeft + r.w * view.scale
        readonly property real cBottom: cTop + r.h * view.scale
        readonly property real imgLeft: view.offset.x
        readonly property real imgTop: view.offset.y
        readonly property real imgRight: view.offset.x + stage.imageSize.width * view.scale
        readonly property real imgBottom: view.offset.y + stage.imageSize.height * view.scale

        Rectangle {
            x: cropOverlay.cLeft
            y: cropOverlay.cTop
            width: cropOverlay.cRight - cropOverlay.cLeft
            height: cropOverlay.cBottom - cropOverlay.cTop
            color: "transparent"
            border.width: 2
            border.color: TelamonStyle.accent
        }

        Repeater {
            model: [[0, 0], [0.5, 0], [1, 0], [0, 0.5], [1, 0.5], [0, 1], [0.5, 1], [1, 1]]
            Rectangle {
                required property var modelData
                width: 10
                height: 10
                radius: TelamonStyle.radiusSmall
                x: cropOverlay.cLeft + modelData[0] * (cropOverlay.cRight - cropOverlay.cLeft) - width / 2
                y: cropOverlay.cTop + modelData[1] * (cropOverlay.cBottom - cropOverlay.cTop) - height / 2
                color: TelamonStyle.accent
                border.width: 1
                border.color: "white" // telamon-lint: allow-raw handle outline over any picture
            }
        }

        // The size of the crop.
        Rectangle {
            x: Math.max(cropOverlay.imgLeft, Math.min(cropOverlay.cRight - width, cropOverlay.cLeft))
            y: Math.min(cropOverlay.imgBottom - height - 4, cropOverlay.cBottom + 6)
            width: sizeLabel.implicitWidth + 12
            height: sizeLabel.implicitHeight + 4
            radius: TelamonStyle.radiusSmall
            color: TelamonStyle.surfaceRaised
            border.width: 1
            border.color: TelamonStyle.separator
            visible: stage.cropDraft !== null
            TelamonLabel {
                id: sizeLabel
                anchors.centerIn: parent
                textStyle: TelamonLabel.Caption
                text: qsTr("%1 × %2").arg(cropOverlay.r.w).arg(cropOverlay.r.h)
            }
        }
    }

    // The text being typed, at the size it will have.
    Item {
        id: textBox
        visible: stage.editing
        x: view.offset.x + (stage.editX - stage.shown.x) * view.scale
        y: view.offset.y + (stage.editY - stage.shown.y) * view.scale
        width: Math.max(editor.contentWidth + 8, 24)
        height: Math.max(editor.contentHeight + 4, editor.font.pixelSize * 1.3)

        Rectangle {
            x: -4
            y: -2
            width: parent.width + 4
            height: parent.height + 2
            color: "transparent"
            border.width: 1
            border.color: TelamonStyle.accent
            radius: TelamonStyle.radiusSmall
        }

        TextEdit {
            id: editor
            width: parent.width
            textFormat: TextEdit.PlainText
            wrapMode: TextEdit.NoWrap
            selectByMouse: true
            color: stage.drawColor
            font.weight: Font.Medium
            font.pixelSize: Math.max(8, stage.textSize * view.scale)
            selectionColor: TelamonStyle.accent
            Accessible.role: Accessible.EditableText
            Accessible.name: qsTr("Text annotation")
            Accessible.description: qsTr("Type the text. Ctrl+Enter or a click elsewhere places it, Escape discards it.")

            Keys.onEscapePressed: event => {
                stage.cancelText();
                event.accepted = true;
            }
            Keys.onPressed: event => {
                if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter) && (event.modifiers & Qt.ControlModifier)) {
                    stage.commitText();
                    event.accepted = true;
                }
            }
        }
    }

    MouseArea {
        id: area
        anchors.fill: parent
        acceptedButtons: Qt.LeftButton
        hoverEnabled: true
        enabled: stage.hasImage
        z: -1 // under the text box, so it takes its own clicks
        cursorShape: stage.tool === "text" ? Qt.IBeamCursor
            : stage.tool === "crop" ? stage.cropCursor(stage.hoverCursorTool.substring(5)) : Qt.CrossCursor
        onPressed: mouse => stage.press(mouse.x, mouse.y)
        onPositionChanged: mouse => stage.move(mouse.x, mouse.y)
        onReleased: mouse => stage.release(mouse.x, mouse.y)
        onCanceled: {
            stage._pressed = false;
            stage.draft = null;
            stage.cropDraft = null;
        }
    }
}
