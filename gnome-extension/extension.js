import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Shell from 'gi://Shell';

const SERVICE_NAME = 'org.wayfrost.Capture';
const OBJECT_PATH = '/org/wayfrost/Capture';

const CAPTURE_XML = `
<node>
  <interface name="${SERVICE_NAME}">
    <method name="CaptureScreen">
      <arg name="ok" type="b" direction="out"/>
      <arg name="data" type="ay" direction="out"/>
    </method>
  </interface>
</node>
`;

const WayfrostCaptureDBus = GObject.registerClass(
class WayfrostCaptureDBus extends GObject.Object {
    constructor() {
        super();
        this._dbusObject = Gio.DBusExportedObject.wrapJSObject(CAPTURE_XML, this);
        this._dbusObject.export(Gio.DBus.session, OBJECT_PATH);
        this._nameId = Gio.DBus.session.own_name(
            SERVICE_NAME,
            Gio.BusNameOwnerFlags.NONE,
            null,
            () => console.log(`[Wayfrost] Lost DBus name ${SERVICE_NAME}`)
        );
    }

    destroy() {
        if (this._nameId) {
            Gio.DBus.session.unown_name(this._nameId);
            this._nameId = 0;
        }
        this._dbusObject?.unexport();
        this._dbusObject?.run_dispose();
        this._dbusObject = null;
    }

    async CaptureScreenAsync(_params, invocation) {
        try {
            const shooter = new Shell.Screenshot();
            // Screenshot into a temp file in gnome-shell's OWN tmp dir, read the
            // PNG bytes back, delete the file, and return the bytes over D-Bus.
            // The file never leaves the shell process, so the Flatpak sandbox
            // shares no filesystem path with gnome-shell — it only receives bytes.
            const tmpPath = GLib.build_filenamev([
                GLib.get_tmp_dir(),
                `wayfrost_${GLib.get_monotonic_time()}.png`,
            ]);
            const file = Gio.File.new_for_path(tmpPath);
            const stream = file.replace(null, false, Gio.FileCreateFlags.NONE, null);
            await shooter.screenshot(false, stream);
            stream.close(null);
            const bytes = GLib.file_get_contents(tmpPath)[1];
            GLib.remove(tmpPath);
            invocation.return_value(new GLib.Variant('(bay)', [true, bytes]));
        } catch (error) {
            console.error(`[Wayfrost] Capture failed: ${error.message}`);
            invocation.return_value(new GLib.Variant('(bay)', [false, new Uint8Array(0)]));
        }
    }
});

export default class WayfrostCaptureExtension extends Extension {
    enable() {
        this._dbusServer = new WayfrostCaptureDBus();
    }

    disable() {
        this._dbusServer?.destroy();
        this._dbusServer = null;
    }
}
