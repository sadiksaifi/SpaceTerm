# Allow confirmed Tab closure across clients

For the server architecture in [issue #162](https://github.com/sadiksaifi/SpaceTerm/issues/162), an authorized SpaceTerm Client can close a Tab containing Panes controlled by another client after Close Confirmation. The confirmation states that the affected Terminal Sessions end for every client. [Pane control](0022-give-one-client-control-of-each-pane.md) governs input and dimensions, so the closing client does not need to take over those Panes first.
