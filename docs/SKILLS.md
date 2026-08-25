# Agent Skills

Albatross implements the open [Agent Skills specification](https://github.com/agentskills/agentskills/blob/main/docs/specification.mdx). A skill is a directory containing an exact `SKILL.md` filename with YAML frontmatter and Markdown instructions. Supporting scripts, references, and assets can live beside it.

## Create a skill

Create `review/SKILL.md` in one of the discovery roots:

```markdown
---
name: review
description: Review code changes for correctness, regressions, and missing tests.
license: MIT
compatibility: Requires git.
metadata:
  author: acme
---

# Review

Inspect the diff, trace changed behavior, and report findings by severity.
Read `references/checklist.md` when a full release review is requested.
```

`name` and `description` are required. The name must match the containing directory and use 1–64 lowercase ASCII letters, numbers, or single hyphens. Descriptions must contain 1–1024 characters. Albatross also parses the standard optional `license`, `compatibility`, `metadata`, and experimental `allowed-tools` fields.

## Discovery and precedence

Albatross discovers exact `SKILL.md` files in these locations, from highest to lowest precedence:

1. `<project>/.albatross/skills/`
2. `<project>/.agents/skills/`
3. installed npm and Git packages
4. `$XDG_CONFIG_HOME/albatross/skills/` (normally `~/.config/albatross/skills/`)
5. `~/.agents/skills/`

When two skills have the same standard name, the higher-precedence skill is used and `/skills` reports the shadowing diagnostic. Packaged skills also retain `/skill:<package>:<name>` aliases so a shadowed package skill remains explicitly addressable.

Restart Albatross after adding, removing, or changing a skill so the catalog is rebuilt.

## Activate a skill

Use `/skills` to inspect the discovered catalog and validation diagnostics. Explicitly activate a skill with:

```text
/skill:review review the authentication changes
```

Slash completion discovers every valid skill and its packaged aliases. An explicit slash command loads the full `SKILL.md` instructions and starts a normal agent turn with the optional task.

Albatross also exposes skill names and descriptions to the model. When a task matches, the model can progressively load the full instructions through the `activate_skill` tool. Supporting resource paths are listed at activation time, while their contents remain unloaded until needed.

Project-local skills contain repository-controlled instructions, so model-initiated activation requires approval. Explicit `/skill:name` activation is treated as direct user consent. The experimental `allowed-tools` field is surfaced as metadata only; it never bypasses Albatross tool selection or approval policy.

## Packages

Packages can point their `albatross.skills` manifest entry at one or more skill directories. Each discovered skill must still follow the standard directory layout and frontmatter rules. See [PACKAGES.md](PACKAGES.md) for installation and package-authoring details.
