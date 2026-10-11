# Keep Tab selection independent per client

For the server architecture in [issue #162](https://github.com/sadiksaifi/SpaceTerm/issues/162), each SpaceTerm Client selects its Active Tab independently. A client opening the Logs Tab leaves another client viewing the Build Tab, so clients can work in different Tabs without moving each other's view. Both clients still access the Tabs owned by the same SpaceTerm Server.

[ADR 0042](0042-keep-tab-order-and-pane-layouts-specific-to-each-client.md) extends this independence to Tab display order and Pane Layouts.
