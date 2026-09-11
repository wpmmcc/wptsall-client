// Minimal probe: does the GTK icon path populate _NET_WM_ICON in a given
// environment? Run with:
//   env -u WAYLAND_DISPLAY GDK_BACKEND=x11 DISPLAY=:0 \
//     cargo run --example icon_probe
// then check `xprop -id <id> _NET_WM_ICON` on the "Icon Probe" window.
//
// Empirical result on GNOME (Wayland session + XWayland, GTK 3.24,
// 2026-09-11): NEITHER gtk_window_set_icon NOR gdk_window_set_icon_list
// (applied on realize) produces _NET_WM_ICON — the property stays absent.
// Taskbar icons on modern GNOME therefore come from the .desktop entry +
// hicolor theme (bundled installs get those from the Tauri bundler), and
// unbundled `cargo run` sessions show a generic window icon. Keep this
// probe to re-verify behavior on other distros/WMs (KDE, Xfce) where the
// GDK path may still work.
use gtk::prelude::*;

fn main() {
    let app = gtk::Application::new(None, gtk::gio::ApplicationFlags::empty());
    app.connect_activate(|app| {
        let win = gtk::ApplicationWindow::new(app);
        win.set_title("Icon Probe");
        win.set_default_size(320, 200);
        let png: &[u8] = include_bytes!("../icons/128x128.png");
        match gtk::gdk_pixbuf::Pixbuf::from_read(png) {
            Ok(pixbuf) => {
                win.set_icon(Some(&pixbuf));
                println!("probe: set_icon called with {}x{}", pixbuf.width(), pixbuf.height());
                // GTK's realize path may not write _NET_WM_ICON in this
                // environment; drive the GdkWindow directly.
                let gwin = win.window();
                match gwin {
                    Some(gdk_window) => {
                        gdk_window.set_icon_list(&[pixbuf]);
                        println!("probe: gdk set_icon_list applied on realized window");
                    }
                    None => {
                        println!("probe: window not realized yet; deferring via realize signal");
                        win.connect_realize(move |w| {
                            if let Some(gdk_window) = w.window() {
                                let pixbuf = pixbuf.clone();
                                gdk_window.set_icon_list(&[pixbuf]);
                                println!("probe: gdk set_icon_list applied on realize");
                            }
                        });
                    }
                }
            }
            Err(e) => println!("probe: pixbuf decode failed: {e}"),
        }
        win.show_all();
    });
    app.run();
}
