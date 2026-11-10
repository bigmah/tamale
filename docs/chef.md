# The chef

The tamale chef is an agent that looks after your other sessions instead of working in a project.
Ask it what everyone is working on, which sessions need you, or whether the tests pass
everywhere. It finds out from the sessions themselves, on every machine that uses your
[hub key](remote-access.md).

```sh
tamale chef
```

That starts the chef, or goes back to it if it's already running. Inside any session, `/chef` or
the footer's **🫔 Chef** tab does the same, leaving that session running behind it. The chef is a
background session like any other: `ctrl+c` detaches, `/sessions` goes back to a session, and
`tamale chef` comes back to it.

## What it can do

The chef has four tools, and none of the usual ones: it doesn't edit files or run commands.

| Tool | What it does | Asks first? |
| --- | --- | --- |
| `list_sessions` | Every session on the hub: where it runs, its title, model, and whether it's working, waiting for approval, idle or offline. Also notes sessions on this machine that the hub can't reach. | No |
| `read_session` | A session's recent transcript and status, without disturbing it. | No |
| `ask_sessions` | A **side question** to one or more sessions, or `all` of them, asked at once. | No |
| `message_session` | A real prompt into a session, which acts on it. By default it waits for the reply. | In `ask` and `accept-edits` modes |

A side question is answered by the session's own agent, from its whole conversation, on a copy
of its history. It can read and search files to answer, but edits and shell commands are refused,
and nothing is added to its conversation, so it's safe to ask a session in the middle of a turn.
The session's transcript notes `🫔 The chef asked: …` so whoever is watching it knows. A message,
on the other hand, joins the session's conversation as `🫔 From the tamale chef: …`, and its agent
acts on it like anything you type there.

Side questions run on the asked session's backend and model, and are paid for there.

## How it looks

**In the terminal**, the chef greets you with suggestions above an empty conversation, and keeps
a **Kitchen** panel beside the chat (on terminals at least 100 columns wide) with every session:
what needs you first, then what's working, then the rest. It refreshes every few seconds.

**On the web and on your phone**, the chef's page is tamale's web UI in amber, with the same
kitchen as a column of cards. Tap a card to ask what that session is doing, or use **Catch me up**,
**Ask…** and, on a session waiting for approval, **What does it need?**. On a phone the kitchen
slides in from the 🫔 button in the top bar, which shows a red count when sessions need you. The
chef connects to the hub as soon as it starts, if a key is set, so it's in your session list on
[tamale-hub.com](https://tamale-hub.com), pinned to the top.

## What it needs

- `TAMALE_HUB_KEY` set where the chef runs, and on every machine whose sessions it should see.
- Each session reachable through the hub: `/remote` in it, or `[hub] connect = "auto"` so they all
  are. The chef lists sessions on its own machine that aren't, so you know to turn it on.
- Sessions recent enough to answer side questions. An older tamale still lists, reads and takes
  messages; the chef says when one can't answer.
- End-to-end encryption, which is on by default. The chef won't talk to a session with
  `[hub] e2e = false`.

The chef talks to sessions over the same encrypted channel as the web UI and `/sessions`, so the
hub can't read what it asks or what they answer.

> [!WARNING]
> In the default `auto` permission mode, the chef can message any session without asking, and
> those sessions act on what it sends. Use `ask` mode (`shift+tab`, or `/permissions ask`) if you
> want to approve each message first.
