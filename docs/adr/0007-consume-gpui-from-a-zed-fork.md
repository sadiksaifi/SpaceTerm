# Consume GPUI from a Zed fork

SpaceTerm pins GPUI and its platform crates to a SpaceTerm fork of Zed so patches remain owned
and validated with their dependency. Vendoring Zed or adding it as a submodule would make updates
and ownership harder to track. Toolchain updates follow the fork because both build the same
dependencies.
