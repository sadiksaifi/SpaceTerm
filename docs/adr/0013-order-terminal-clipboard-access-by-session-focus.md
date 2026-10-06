# Order terminal clipboard access by Terminal Session focus

Clipboard Authority belongs to the originating Pane's Terminal Input Focus, and reads need explicit permission, because terminal output can come from local or remote programs. Clipboard requests use a bounded asynchronous channel because blocking the worker on a UI reply would deadlock a synchronous Selection query.
