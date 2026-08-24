# Albatross packages

Albatross packages bundle extensions, skills, prompt templates, and terminal
themes for distribution through npm or Git. Packages are installed globally
under the Albatross config directory and are available in every workspace.

## Install and manage

```bash
albatross install npm:@acme/albatross-tools
albatross install npm:@acme/albatross-tools@1.2.0
albatross install git:https://github.com/acme/albatross-tools
albatross install git:https://github.com/acme/albatross-tools@v1

albatross list
albatross update
albatross update @acme/albatross-tools
albatross remove @acme/albatross-tools
```

An npm version or Git ref makes a package pinned, so `albatross update` skips
it. Re-run `albatross install` with a different source to replace it.

Git refs may use either `@ref` or `#ref`. GitHub shorthand such as
`git:github.com/acme/albatross-tools` is expanded to HTTPS.

The registry is stored at `~/.config/albatross/packages.json` (or under
`$XDG_CONFIG_HOME`). Installed content lives beside it in `packages/npm/` and
`packages/git/`. `/packages` shows packages and resource counts in an
interactive session. Restart Albatross after installing, updating, or removing
a package so the session can rebuild its resource registry.

## Security

Treat packages as code and instructions, not passive data.

- npm and Git dependency installation always uses `npm install
  --ignore-scripts`, so package and dependency lifecycle scripts do not run.
- Git clone still invokes your configured Git transport and credential helper.
- Skills and prompts can influence model behavior. Review them before invoking
  `/skill:<package>:<name>` or `/prompt run <package>:<name>`.
- Themes are parsed as data containing only 256-color indexes.
- Package extensions do not execute immediately. They enter the same
  configuration-hash trust flow as project extensions; review with
  `/extensions`, then run `/extensions trust <package>--<extension>`.
- Updating a package changes its resolved extension configuration and revokes
  the previous workspace trust decision.

## Create a package

Every package must contain `package.json`. Add an `albatross` manifest:

```json
{
  "name": "@acme/albatross-tools",
  "version": "1.0.0",
  "keywords": ["albatross-package"],
  "albatross": {
    "extensions": [
      {
        "name": "review-tools",
        "command": "python3",
        "args": ["extensions/review.py"]
      }
    ],
    "skills": ["skills"],
    "prompts": ["prompts"],
    "themes": ["themes"]
  }
}
```

Resource paths are relative to the package root and must remain inside it. In
v1, each path names a file or directory; glob patterns are not supported.

If the arrays are omitted, Albatross discovers conventional directories:

- `extensions/` — each `.json` file is an extension descriptor using the same
  fields as an entry in `albatross.extensions`
- `skills/` — recursively discovers `SKILL.md` and other Markdown files
- `prompts/` — recursively discovers Markdown files
- `themes/` — recursively discovers JSON theme files

For ecosystem portability, Albatross also reads `skills`, `prompts`, and
`themes` paths from a Pi `pi` manifest when the corresponding `albatross` path
is absent. Pi TypeScript extensions are not loaded: Albatross extensions are
language-neutral JSON-RPC subprocesses and need an `albatross.extensions`
descriptor.

An extension command beginning with `./` is resolved to an absolute path
inside the package. Other commands are resolved through `PATH`. Extension
arguments run with the package root as their working directory.

## Skills

A skill is Markdown. `skills/review/SKILL.md` in package `acme-tools` becomes:

```text
/skill:acme-tools:review <optional task>
```

The command starts a normal agent turn with the skill instructions and the
optional task. `/skills` lists installed skills. Package names and resource
names are normalized for command safety.

## Prompt templates

Markdown prompts are namespaced the same way:

```text
/prompt run acme-tools:review
```

`/prompt list` includes packaged prompts alongside saved and built-in prompts.

## Themes

A theme is a JSON object containing a globally unique name and ANSI 256-color
indexes:

```json
{
  "name": "ocean",
  "accent": 51,
  "accentDeep": 37,
  "muted": 244,
  "success": 84,
  "warn": 220,
  "error": 203,
  "magenta": 213,
  "fade": [51, 45, 39, 38, 37, 31, 30, 24, 23, 237, 235, 234]
}
```

Only `name` is required; omitted colors use the cyan defaults. `fade` must
contain exactly twelve color indexes. Use `/theme` to list available themes and
`/theme ocean` to apply and persist one for the current project.

## Example

[`examples/package`](../examples/package) contains one resource of every type.
Publish that directory to npm or push it to Git, then install it with the
corresponding source command.
