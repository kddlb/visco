//! visco: step through how codecs encode and decode, stage by stage.

mod app;
mod hexview;
mod inspector;
mod raster;
mod viewer;

use std::{path::PathBuf, sync::Arc};

use gpui_kit::component::Theme;
use gpui_kit::*;

use app::Visco;

actions!(visco, [Open, Quit]);

fn main() {
    let initial: Option<PathBuf> = std::env::args_os().nth(1).map(PathBuf::from);

    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        cx.bind_keys([KeyBinding::new("cmd-o", Open, None), KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus(vec![
            Menu { name: "visco".into(), items: vec![MenuItem::action("Quit visco", Quit)], disabled: false },
            Menu { name: "File".into(), items: vec![MenuItem::action("Open…", Open)], disabled: false },
        ]);

        let options = WindowOptions {
            titlebar: Some(TitlebarOptions { title: Some("visco".into()), ..Default::default() }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(920.)), cx))),
            ..Default::default()
        };
        let initial = initial.clone();
        gpui_kit::open_window(options, cx, move |window, cx| {
            Theme::sync_system_appearance(Some(window), cx);
            let view = cx.new(|cx| Visco::new(Arc::new(visco_jpeg::JpegCodec), window, cx));
            let weak = view.downgrade();
            cx.on_action(move |_: &Open, cx| {
                weak.update(cx, |this, cx| this.prompt_open(cx)).ok();
            });
            if let Some(p) = initial {
                view.update(cx, |this, cx| this.open(p, cx));
            }
            view
        })
        .expect("failed to open window");
        cx.activate(true);
    });
}
