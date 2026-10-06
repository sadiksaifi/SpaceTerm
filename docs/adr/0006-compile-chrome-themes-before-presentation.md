# Compile Chrome themes before presentation

Sparse built-in Chrome definitions compile before presentation so derived colors follow one set
of authored decisions. Presentation adapts compiled paints to surfaces and accessibility without
changing those definitions, and Terminal Themes retain separate protocol-owned colors. Floating
surfaces stay in GPUI because native popup views would split interaction, accessibility, and
lifecycle ownership across platforms. Translucency exposes desktop pixels GPUI cannot sample,
so opaque accessibility presentation remains the deterministic contrast fallback.
