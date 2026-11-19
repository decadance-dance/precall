# Resolve: slack

Each entry of `refs` is a Slack message reference `slack:<channel-id>/<ts>`: the id of a channel or DM and a message's `ts`. Fetch each message through your Slack read tools (read the channel or the thread around that `ts`) and return its text exactly as Slack gives it, with its author's name in front (`<name>: <text>`).

Return `texts` with one entry per reference you were given, and no other: the message's text, or `null` when you cannot find or read that message. Never guess or reconstruct a text.
