pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import Telamon.Ui

// The editor window: a header with the file actions, one slim row of tools,
// and the picture. Save and Copy go through the telamon-screenshot CLI.
TelamonWindow {
    id: root

    // The image to open: a path, "-" for stdin, or empty.
    required property string startSource

    // For the test build's scenarios.
    readonly property alias docRef: doc
    readonly property alias backendRef: backend
    readonly property alias stageRef: stage
    readonly property alias newPopoverRef: newPopover

    property string tool: "arrow"
    property int captureDelay: 0
    property bool _forceClose: false

    readonly property bool hasImage: backend.imageSize.width > 0
    readonly property var swatches: [
        { color: "#e5484d", name: qsTr("Red") }, // telamon-lint: allow-raw annotation colours: content of the image, not the interface
        { color: "#f76b15", name: qsTr("Orange") }, // telamon-lint: allow-raw
        { color: "#ffd60a", name: qsTr("Yellow") }, // telamon-lint: allow-raw
        { color: "#30a46c", name: qsTr("Green") }, // telamon-lint: allow-raw
        { color: "#3e63dd", name: qsTr("Blue") }, // telamon-lint: allow-raw
        { color: "#8e4ec6", name: qsTr("Purple") }, // telamon-lint: allow-raw
        { color: "#ffffff", name: qsTr("White") }, // telamon-lint: allow-raw
        { color: "#000000", name: qsTr("Black") } // telamon-lint: allow-raw
    ]
    // Each tool keeps its own colour: red arrows, a yellow highlighter, black redaction.
    property var toolColorIndex: ({ arrow: 0, rect: 0, pen: 0, highlighter: 2, text: 0, marker: 0, redact: 7, crop: 0 })
    property int sizeIndex: 1

    readonly property var tools: [
        { id: "crop", text: qsTr("Crop") },
        { id: "arrow", text: qsTr("Arrow") },
        { id: "rect", text: qsTr("Rectangle") },
        { id: "pen", text: qsTr("Pen") },
        { id: "highlighter", text: qsTr("Highlighter") },
        { id: "text", text: qsTr("Text") },
        { id: "marker", text: qsTr("Numbered Marker") },
        { id: "redact", text: qsTr("Redact") }
    ]

    title: qsTr("Screenshot") + (doc.needsSave ? " " + qsTr("[modified]") : "")
    width: Kirigami.Units.gridUnit * 56
    height: Kirigami.Units.gridUnit * 38
    minimumWidth: Kirigami.Units.gridUnit * 34
    minimumHeight: Kirigami.Units.gridUnit * 22
    visible: true
    LayoutMirroring.enabled: Qt.application.layoutDirection === Qt.RightToLeft
    LayoutMirroring.childrenInherit: true

    function cropRect() {
        const c = doc.crop;
        return c ? Qt.rect(c.x, c.y, c.w, c.h) : Qt.rect(0, 0, 0, 0);
    }

    function fileName(path) {
        return backend.baseName(path);
    }

    function setColorIndex(i) {
        const t = Object.assign({}, root.toolColorIndex);
        t[root.tool] = i;
        root.toolColorIndex = t;
        stage.refocus();
    }

    function showError(title, text) {
        errorLoader.title = title;
        errorLoader.text = text;
        errorLoader.active = true;
        errorLoader.item.open();
    }

    // Asks before throwing away work that is not saved, then runs `then`.
    function discardThen(then) {
        stage.finishEditing();
        if (!doc.needsSave) {
            then();
            return;
        }
        root.confirm({
            title: qsTr("Discard Changes?"),
            text: qsTr("This screenshot has changes that are not saved. If you go on, they are lost."),
            acceptText: qsTr("Discard"),
            rejectText: qsTr("Cancel"),
            destructive: true
        }, ok => {
            if (ok)
                then();
        });
    }

    function requestClose() {
        root.close();
    }

    function save() {
        if (!root.hasImage || backend.helperBusy)
            return;
        stage.finishEditing();
        saveStateId = doc.stateId();
        backend.save(doc.items, root.cropRect());
    }

    function copy() {
        if (!root.hasImage || backend.helperBusy)
            return;
        stage.finishEditing();
        backend.copy(doc.items, root.cropRect());
    }

    property int saveStateId: 0

    function chooseSaveAs() {
        if (!root.hasImage)
            return;
        stage.finishEditing();
        saveAsLoader.active = true;
        saveAsLoader.item.openWith(backend.suggestedSavePath());
    }

    function doSaveAs(typed) {
        const dlg = saveAsLoader.item;
        const r = backend.resolveSavePath(typed);
        if (r.error) {
            dlg.errorText = r.error;
            return;
        }
        const write = () => {
            const id = doc.stateId();
            const err = backend.saveAs(r.path, doc.items, root.cropRect());
            if (err) {
                dlg.errorText = err;
                return;
            }
            doc.markSaved(id);
            dlg.close();
            root.toast(qsTr("Saved as %1").arg(root.fileName(r.path)));
        };
        if (r.exists) {
            root.confirm({
                title: qsTr("Replace File?"),
                text: qsTr("The file %1 already exists. Replacing it cannot be undone.").arg(root.fileName(r.path)),
                acceptText: qsTr("Replace"),
                rejectText: qsTr("Cancel"),
                destructive: true
            }, ok => {
                if (ok)
                    write();
            });
        } else {
            write();
        }
    }

    function chooseOpen() {
        root.discardThen(() => {
            openLoader.active = true;
            openLoader.item.openWith("");
        });
    }

    function doOpen(typed) {
        const dlg = openLoader.item;
        const r = backend.resolveOpenPath(typed);
        if (r.error) {
            dlg.errorText = r.error;
            return;
        }
        dlg.close();
        backend.load(r.path);
    }

    function startCapture(mode, delay) {
        newPopover.close();
        root.discardThen(() => {
            launchTimer.mode = mode;
            launchTimer.delay = delay;
            // Out of the picture before the CLI looks at the screen.
            root.hide();
            launchTimer.start();
        });
    }

    Timer {
        id: launchTimer
        property string mode
        property int delay
        interval: 300
        onTriggered: {
            if (backend.startCapture(mode, delay)) {
                root._forceClose = true;
                Qt.quit();
            } else {
                root.show();
                root.requestActivate();
                root.showError(qsTr("Could Not Start a Screenshot"), qsTr("telamon-screenshot could not be started."));
            }
        }
    }

    onClosing: close => {
        stage.finishEditing();
        if (!root._forceClose && doc.needsSave) {
            close.accepted = false;
            root.confirm({
                title: qsTr("Discard Changes?"),
                text: qsTr("This screenshot has changes that are not saved. If you close it, they are lost."),
                acceptText: qsTr("Discard"),
                rejectText: qsTr("Cancel"),
                destructive: true
            }, ok => {
                if (ok) {
                    root._forceClose = true;
                    root.close();
                }
            });
        }
    }

    Component.onCompleted: {
        if (startSource.length > 0)
            backend.load(startSource);
    }

    Document {
        id: doc
    }

    Backend {
        id: backend
        onLoaded: doc.reset()
        onLoadFailed: message => root.showError(qsTr("Could Not Open the Image"), message)
        onHelperFinished: (kind, ok, text) => {
            if (kind === "save") {
                if (ok) {
                    doc.markSaved(root.saveStateId);
                    root.toast(qsTr("Saved as %1").arg(root.fileName(text)));
                } else {
                    root.toast(qsTr("Could not save: %1").arg(text), { kind: "error" });
                }
            } else {
                if (ok)
                    root.toast(qsTr("Copied to the clipboard"));
                else
                    root.toast(qsTr("Could not copy: %1").arg(text), { kind: "error" });
            }
        }
    }

    // ---- actions ---------------------------------------------------------

    component ToolAction: TelamonAction {
        id: act
        required property string toolId
        checkable: true
        section: qsTr("Tools")
        enabled: root.hasImage
        onTriggered: {
            root.tool = act.toolId;
            act.checked = true;
        }
        property Binding sync: Binding {
            target: act
            property: "checked"
            value: root.tool === act.toolId
        }
    }

    ToolAction { id: cropAction; toolId: "crop"; text: qsTr("Crop"); symbol: Symbols.Crop; shortcut: "C" }
    ToolAction { id: arrowAction; toolId: "arrow"; text: qsTr("Arrow"); symbol: Symbols.NorthEast; shortcut: "A" }
    ToolAction { id: rectAction; toolId: "rect"; text: qsTr("Rectangle"); symbol: Symbols.Rectangle; shortcut: "R" }
    ToolAction { id: penAction; toolId: "pen"; text: qsTr("Pen"); symbol: Symbols.Draw; shortcut: "P" }
    ToolAction { id: highlighterAction; toolId: "highlighter"; text: qsTr("Highlighter"); symbol: Symbols.InkHighlighter; shortcut: "H" }
    ToolAction { id: textAction; toolId: "text"; text: qsTr("Text"); symbol: Symbols.Title; shortcut: "T" }
    ToolAction { id: markerAction; toolId: "marker"; text: qsTr("Numbered Marker"); symbol: Symbols.Counter1; shortcut: "N" }
    ToolAction { id: redactAction; toolId: "redact"; text: qsTr("Redact"); symbol: Symbols.HideSource; shortcut: "X" }

    TelamonAction {
        id: newAction
        text: qsTr("New Screenshot")
        symbol: Symbols.AddAPhoto
        shortcut: "Ctrl+N"
        section: qsTr("File")
        popover: newPopover
        // A click on the button opens the popover itself; the key does it here.
        onTriggered: newPopover.open()
    }
    TelamonAction {
        id: openAction
        text: qsTr("Open Image...")
        symbol: Symbols.FolderOpen
        shortcut: "Ctrl+O"
        section: qsTr("File")
        onTriggered: root.chooseOpen()
    }
    TelamonAction {
        id: saveAction
        text: qsTr("Save")
        symbol: Symbols.Save
        shortcut: "Ctrl+S"
        section: qsTr("File")
        enabled: root.hasImage && !backend.helperBusy
        onTriggered: root.save()
    }
    TelamonAction {
        id: saveAsAction
        text: qsTr("Save As...")
        symbol: Symbols.SaveAs
        shortcut: "Ctrl+Shift+S"
        section: qsTr("File")
        enabled: root.hasImage
        onTriggered: root.chooseSaveAs()
    }
    TelamonAction {
        id: copyAction
        text: qsTr("Copy")
        symbol: Symbols.ContentCopy
        shortcut: "Ctrl+C"
        section: qsTr("File")
        enabled: root.hasImage && !backend.helperBusy
        onTriggered: root.copy()
    }
    TelamonAction {
        id: undoAction
        text: qsTr("Undo")
        symbol: Symbols.Undo
        shortcut: "Ctrl+Z"
        section: qsTr("Edit")
        enabled: doc.canUndo
        onTriggered: {
            stage.cancelGesture();
            doc.undo();
        }
    }
    TelamonAction {
        id: redoAction
        text: qsTr("Redo")
        symbol: Symbols.Redo
        shortcut: "Ctrl+Shift+Z"
        section: qsTr("Edit")
        enabled: doc.canRedo
        onTriggered: {
            stage.cancelGesture();
            doc.redo();
        }
    }
    TelamonAction {
        id: closeAction
        text: qsTr("Close")
        symbol: Symbols.Close
        shortcut: "Ctrl+W"
        section: qsTr("File")
        onTriggered: root.requestClose()
    }
    TelamonAction {
        id: quitAction
        text: qsTr("Quit")
        shortcut: "Ctrl+Q"
        section: qsTr("File")
        onTriggered: root.requestClose()
    }
    // The second key of Redo, the common one on other systems.
    Shortcut {
        sequence: "Ctrl+Y"
        enabled: doc.canRedo
        onActivated: redoAction.trigger()
    }

    header: TelamonHeaderBar {
        leading: TelamonAppMenu {
            menus: [
                { title: qsTr("File"), actions: [newAction, openAction, null, saveAction, saveAsAction, copyAction, null, closeAction] },
                { title: qsTr("Edit"), actions: [undoAction, redoAction] },
                { title: qsTr("Tools"), actions: [cropAction, arrowAction, rectAction, penAction, highlighterAction, textAction, markerAction, redactAction] }
            ]
        }
        actions: [newAction, undoAction, redoAction]
        trailing: [
            SecondaryButton {
                Layout.alignment: Qt.AlignVCenter
                visible: root.hasImage
                action: copyAction
                enabled: copyAction.enabled
            },
            TelamonSplitButton {
                Layout.alignment: Qt.AlignVCenter
                visible: root.hasImage
                action: saveAction
                prominent: true
                ContextMenuItem {
                    action: saveAsAction
                }
            }
        ]
    }

    // ---- content -----------------------------------------------------------

    Item {
        id: content
        anchors.fill: parent
        focus: true

        Keys.onEscapePressed: event => {
            if (!stage.cancelGesture())
                root.requestClose();
            event.accepted = true;
        }

        ColumnLayout {
            anchors.fill: parent
            spacing: 0

            // One slim row: the tools, the colour, the size.
            Item {
                id: toolRow
                Layout.fillWidth: true
                implicitHeight: toolFlow.implicitHeight + TelamonStyle.spacing * 2
                visible: root.hasImage

                Rectangle {
                    anchors.fill: parent
                    color: TelamonStyle.chromeBackground
                }
                Rectangle {
                    anchors.bottom: parent.bottom
                    width: parent.width
                    height: 1
                    color: TelamonStyle.separator
                }

                RowLayout {
                    id: toolFlow
                    anchors.fill: parent
                    anchors.leftMargin: TelamonStyle.spacingLarge
                    anchors.rightMargin: TelamonStyle.spacingLarge
                    anchors.topMargin: TelamonStyle.spacing
                    anchors.bottomMargin: TelamonStyle.spacing
                    spacing: TelamonStyle.spacingLarge

                    Row {
                        spacing: TelamonStyle.spacingXSmall
                        Repeater {
                            model: [cropAction, arrowAction, rectAction, penAction, highlighterAction, textAction, markerAction, redactAction]
                            ToolbarButton {
                                required property var modelData
                                action: modelData
                                focusable: true
                            }
                        }
                    }

                    Rectangle {
                        Layout.preferredWidth: 1
                        Layout.preferredHeight: Kirigami.Units.gridUnit
                        color: TelamonStyle.separator
                    }

                    TelamonAccentPicker {
                        model: root.swatches
                        currentIndex: root.toolColorIndex[root.tool]
                        enabled: root.tool !== "crop"
                        onActivated: index => root.setColorIndex(index)
                        Accessible.name: qsTr("Colour")
                    }

                    Rectangle {
                        Layout.preferredWidth: 1
                        Layout.preferredHeight: Kirigami.Units.gridUnit
                        color: TelamonStyle.separator
                    }

                    TelamonSegmentedControl {
                        model: [
                            { text: qsTr("S"), toolTip: qsTr("Small") },
                            { text: qsTr("M"), toolTip: qsTr("Medium") },
                            { text: qsTr("L"), toolTip: qsTr("Large") }
                        ]
                        currentIndex: root.sizeIndex
                        enabled: root.tool !== "crop" && root.tool !== "redact"
                        onActivated: index => {
                            root.sizeIndex = index;
                            stage.refocus();
                        }
                        Accessible.name: qsTr("Stroke size")
                    }

                    Item {
                        Layout.fillWidth: true
                    }
                }
            }

            Item {
                Layout.fillWidth: true
                Layout.fillHeight: true

                CanvasStage {
                    id: stage
                    anchors.fill: parent
                    anchors.margins: TelamonStyle.spacingLarge
                    visible: root.hasImage
                    document: doc
                    backend: backend
                    tool: root.tool
                    drawColor: root.swatches[root.toolColorIndex[root.tool]].color
                    sizeIndex: root.sizeIndex
                    toolName: {
                        for (const t of root.tools) {
                            if (t.id === root.tool)
                                return t.text;
                        }
                        return "";
                    }
                }

                // Nothing open: take a screenshot or open an image.
                Loader {
                    anchors.centerIn: parent
                    width: Math.min(parent.width - TelamonStyle.spacingXXLarge * 2, Kirigami.Units.gridUnit * 44)
                    active: !root.hasImage && !backend.loading
                    sourceComponent: emptyState
                }
            }
        }
    }

    Component {
        id: emptyState
        ColumnLayout {
            spacing: TelamonStyle.spacingXLarge

            Symbol {
                Layout.alignment: Qt.AlignHCenter
                icon: Symbols.AddAPhoto
                size: Kirigami.Units.iconSizes.huge
                color: TelamonStyle.textMuted
            }
            TelamonLabel {
                Layout.alignment: Qt.AlignHCenter
                text: qsTr("No Screenshot")
                textStyle: TelamonLabel.Title
            }
            TelamonLabel {
                Layout.alignment: Qt.AlignHCenter
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.Wrap
                color: TelamonStyle.textMuted
                text: qsTr("Take a new screenshot, or open an image to draw on.")
            }
            NewScreenshotPanel {
                Layout.alignment: Qt.AlignHCenter
                horizontal: true
                delay: root.captureDelay
                onDelayEdited: d => root.captureDelay = d
                onCaptureRequested: (mode, delay) => root.startCapture(mode, delay)
            }
            SecondaryButton {
                Layout.alignment: Qt.AlignHCenter
                action: openAction
            }
        }
    }

    TelamonPopover {
        id: newPopover
        Loader {
            active: newPopover.visible
            sourceComponent: NewScreenshotPanel {
                delay: root.captureDelay
                onDelayEdited: d => root.captureDelay = d
                onCaptureRequested: (mode, delay) => root.startCapture(mode, delay)
            }
        }
    }

    Loader {
        id: saveAsLoader
        active: false
        sourceComponent: PathDialog {
            saveMode: true
            title: qsTr("Save As")
            acceptText: qsTr("Save")
            placeholder: qsTr("Full path of the new file")
            nameFilters: [qsTr("Images (*.png *.jpg *.jpeg)")]
            onChosen: path => root.doSaveAs(path)
        }
    }

    Loader {
        id: openLoader
        active: false
        sourceComponent: PathDialog {
            title: qsTr("Open Image")
            acceptText: qsTr("Open")
            placeholder: qsTr("Path of an image")
            nameFilters: [qsTr("Images (*.png *.jpg *.jpeg *.webp *.bmp *.gif)"), qsTr("All files (*)")]
            onChosen: path => root.doOpen(path)
        }
    }

    Loader {
        id: errorLoader
        property string title
        property string text
        active: false
        sourceComponent: ConfirmDialog {
            title: errorLoader.title
            text: errorLoader.text
            showReject: false
            acceptText: qsTr("OK")
        }
    }
}
