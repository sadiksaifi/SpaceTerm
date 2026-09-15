# SpaceTerm GPUI patch

Source: the published `gpui` 0.2.2 crate. The upstream Apache 2.0 license is retained.

The macOS renderer used additive destination alpha in the main and path-sprite
pipelines while using source-over RGB. Translucent surfaces therefore accumulated
coverage incorrectly, darkening Light surfaces and creating edge artifacts.
Both pipelines now use `OneMinusSourceAlpha` for destination alpha, matching the
path-rasterization pipeline and premultiplied source-over composition.

Keep this patch limited to those two blend factors. Remove the local patch when
an adopted upstream release supplies the same correction.
