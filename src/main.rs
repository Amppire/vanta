use gpui::{
    App, Application, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    rgb, size,
};

const BACKGROUND: u32 = 0x00fb_faf7;
const BORDER: u32 = 0x00dd_dbd5;
const MUTED: u32 = 0x0073_746f;
const TEXT: u32 = 0x0028_2927;

struct Vanta;

impl Render for Vanta {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .child(
                div()
                    .h(px(38.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px_3()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child("Vanta"),
            )
            .child(
                div().flex_1().flex().items_center().justify_center().child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .child(div().text_xl().child("Start with the editor."))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .child("Open  ·  Edit  ·  Save  ·  Recover"),
                        ),
                ),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(960.0), px(640.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Vanta),
        )
        .expect("Vanta must open its main window");
        cx.activate(true);
    });
}
