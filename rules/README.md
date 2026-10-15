# Rules

The game rules of the challenge as a normative document, enough to build
an agent without reading the engine's code.

| File | Contents |
|---|---|
| `RULES.md` | The game: structure, observation, action, customers, factory, suppliers, reputation, bank, storage, reports, end of game and score. |
| `PROFILE.md` | Every constant of the profile, with its engine name and unit. |
| `TIMING.md` | The daily decision deadline, defaults on timeout or malformed actions, startup and shutdown. |
| `CHANGES.md` | How the 2026 game differs from the 2007 TAC SCM rules document. |

**Profile:** `tac-scm-2026-draft3`, revision 1.
**Engine:** `tac-scm-engine-core` 0.5.1 (semantic identity
`tac-scm-engine-core/0.5.1`), as shipped in `server/`. The runner plays
this profile when started with `TAC_SCM_PROFILE=tac-scm-2026-draft3`.

These documents describe the behaviour of that engine and runner.

The challenge's resource limits, startup grace and submission rules are in
`tournament/SUBMISSION.md`; the tournament format in `tournament/FORMAT.md`;
the message format in `sdk/PROTOCOL.md`.

Status: checked against the engine source, 2026-10-15 (engine 0.5.1).
