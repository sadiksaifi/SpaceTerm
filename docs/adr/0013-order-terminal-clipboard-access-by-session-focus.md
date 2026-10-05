# Order terminal clipboard access by Terminal Session focus

Terminal Clipboard Access belongs to the originating Pane's Terminal Input Focus, with explicit
permission for reads, because terminal output can come from local or remote programs. Clipboard
requests preserve their order relative to output through a bounded asynchronous channel; blocking
the worker on a UI reply would deadlock a synchronous Selection query. The worker keeps serving
input, queries, and shutdown under backpressure, and an ordered nonblocking input queue prevents
clipboard replies, terminal replies, and user input from interleaving bytes.
