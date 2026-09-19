# SpaceTerm GPUI patch

Source: the published `gpui` 0.2.2 crate. The upstream Apache 2.0 license is retained.

The macOS renderer used additive destination alpha in the main and path-sprite
pipelines while using source-over RGB. Translucent surfaces therefore accumulated
coverage incorrectly, darkening Light surfaces and creating edge artifacts.
Both pipelines now use `OneMinusSourceAlpha` for destination alpha, matching the
path-rasterization pipeline and premultiplied source-over composition.

The macOS window exposes its stored traffic-light position for live updates through
`PlatformWindow::set_traffic_light_position` and `Window::set_traffic_light_position`.
The setter accepts a concrete position because SpaceTerm's host geometry is immutable for a
window's lifetime; windows without host geometry leave AppKit's default untouched. SpaceTerm
titlebar height follows Chrome density, so the native buttons must move when
density changes without reopening the window. Other platforms keep the default no-op.

Keep this patch limited to those blend factors and the traffic-light setter. Remove the
local patch when an adopted upstream release supplies the same corrections.
