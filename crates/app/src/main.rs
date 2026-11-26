use gpui_kit::component::{
    button::{Button, ButtonVariants},
    label::Label,
};
use gpui_kit::*;

struct HelloWorld;

impl Render for HelloWorld {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .child(Label::new("Hello, World!"))
            .child(Button::new("hello").primary().label("Click me"))
    }
}

fn main() {
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(|cx| {
        init(cx);
        open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| HelloWorld))
            .expect("Failed to open window");
    });
}
