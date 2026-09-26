import QtQuick
import QtQuick.Window

// Image.Ready describes the decoded PNG, not the GPU texture: Qt may shrink
// it during upload. Check the actual sampler before presenting a grid frame.
// Only a 16×16 result is read back; the radar pixels stay on the GPU.
Item {
    id: check
    width: 16; height: 16
    property Image image: null
    property size expectedSize: Qt.size(0, 0)
    readonly property bool ready: accepted
    readonly property string error: failure
    property bool accepted: false
    property string failure: ""
    property int generation: 0
    property bool pending: false
    property var grab: null

    onImageChanged: restart()
    onExpectedSizeChanged: restart()
    onVisibleChanged: restart()
    Window.onWindowChanged: restart()
    Connections {
        target: check.image
        function onSourceChanged() { check.restart(); }
        function onStatusChanged() { check.restart(); }
    }
    Connections {
        target: check.Window.window
        function onVisibleChanged() { check.restart(); }
        function onSceneGraphInvalidated() { check.restart(); }
    }
    function releaseGrab() {
        if (grab) reader.unloadImage(grab.url);
        grab = null;
    }
    function restart() {
        generation++;
        accepted = false;
        failure = "";
        pending = false;
        deadline.stop();
        releaseGrab();
        Qt.callLater(verify);
    }
    function fail(message) {
        pending = false;
        deadline.stop();
        releaseGrab();
        failure = message;
    }
    function verify() {
        if (!image || !visible || !Window.window || !Window.window.visible
            || accepted || pending || failure) return;
        if (image.status === Image.Error) {
            fail("Radar image could not be loaded.");
            return;
        }
        if (image.status !== Image.Ready || !reader.available || !probe.item) return;
        if (expectedSize.width <= 0 || expectedSize.height <= 0
            || image.sourceSize.width !== expectedSize.width || image.sourceSize.height !== expectedSize.height) {
            fail("Radar image dimensions do not match the frame.");
            return;
        }
        if (GraphicsInfo.api === GraphicsInfo.Software) {
            fail("Radar requires GPU rendering (OpenGL/Vulkan).");
            return;
        }
        pending = true;
        deadline.restart();
        var token = generation;
        if (!probe.item.grabToImage(result => {
            if (token !== generation || !pending) return;
            grab = result;
            reader.loadImage(result.url);
            readResult(); // ItemGrabResult images can be available synchronously.
        }, Qt.size(16, 16))) fail("Could not verify the radar texture on this GPU.");
    }
    function readResult() {
        if (!pending || !grab || !reader.isImageLoaded(grab.url)) return;
        // Context2D's overload dispatch needs a JS string, not a QUrl value.
        var result = reader.getContext("2d").createImageData(String(grab.url));
        if (!result) { fail("Could not read the radar texture check."); return; }
        var pixels = result.data;
        if (pixels.length >= 4 && pixels[0] === 0 && pixels[1] === 255
            && pixels[2] === 0 && pixels[3] === 255) {
            pending = false;
            deadline.stop();
            releaseGrab();
            accepted = true;
        } else if (pixels.length >= 4 && pixels[0] === 255 && pixels[1] === 0
            && pixels[2] === 0 && pixels[3] === 255) {
            fail("This GPU cannot display the native " + expectedSize.width + " × "
                + expectedSize.height + " radar image. Choose another radar source.");
        } else fail("Could not verify the radar texture on this GPU.");
    }
    Timer {
        id: deadline
        interval: 2000
        onTriggered: check.fail("Could not verify the radar texture on this GPU.")
    }
    Loader {
        id: probe
        width: 16; height: 16
        active: !!check.image && check.image.status === Image.Ready
        onLoaded: Qt.callLater(check.verify)
        sourceComponent: ShaderEffect {
            property var sweep: check.image
            property vector2d expected: Qt.vector2d(check.expectedSize.width, check.expectedSize.height)
            fragmentShader: "shaders/texture-check.frag.qsb"
        }
    }
    // Hide the probe from the scene while allowing grabToImage to render it.
    ShaderEffectSource { sourceItem: probe.item; hideSource: true; visible: false; live: false }
    Canvas {
        id: reader
        width: 1; height: 1; opacity: 0
        contextType: "2d"
        onAvailableChanged: Qt.callLater(check.verify)
        onImageLoaded: check.readResult()
    }
}
