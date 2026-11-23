use gpui::{Context, Div, KeyDownEvent, MouseButton, div, prelude::*, rgb};

use crate::ui::controls::button;

use super::Workspace;

impl Workspace {
    pub(super) fn usage_view(&self, cx: &mut Context<Self>) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x11151d))
            .text_color(rgb(0xd9e0ee))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    view.usage.open = false;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_3()
                    .p_4()
                    .bg(rgb(0x171d28))
                    .border_b_1()
                    .border_color(rgb(0x2a3344))
                    .child(button("Volver").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, _, _, cx| {
                            view.usage.open = false;
                            cx.notify();
                        }),
                    ))
                    .child(div().text_lg().child("Historial de uso local"))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(rgb(0xaab8cc))
                            .child(self.usage.status()),
                    )
                    .child(if self.usage.scanning() {
                        div()
                            .text_sm()
                            .text_color(rgb(0xaab8cc))
                            .child("Actualizando…")
                    } else {
                        button("Actualizar").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _, _, cx| {
                                view.usage.scan();
                                cx.notify();
                            }),
                        )
                    }),
            )
            .child(
                div()
                    .id("usage-history")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.usage.content()),
            )
    }
}
