---
name: trello-create
description: Create a Trello card on a board and list with the trello-cli (mheap) command line tool
argument-hint: "<title> [--board <name>] [--list <name>] [--from <markdown file>]"
---
Create one Trello card with the `trello` CLI (`npm install -g trello-cli`, mheap/trello-cli 1.8+).

The arguments are: $ARGUMENTS

## Defaults and conventions

- Board defaults to `AppShapes` (id `65be2d49d367b88f7e35ab55`, in the AppShapes workspace), the
  board Cavalry shares with Brigade. List defaults to `Cavalry`, this project's list on that
  board. Other lists on the board: Brigade, Wanting, Doing, Releasing, Hibernating.
- Titles are plain, without a ticket number. Trello assigns the card number (`idShort`), which
  is what a commit subject cites (`75: ...`) and what appears in the card URL (`/75-slug`). Card
  75 (`Setup repo and SDLC`) is the Cavalry build-out card; a piece of work with a card of its
  own uses that card's number.
- If a markdown file is given (`--from`), its first `# Heading` is the title and the rest is the
  description. Trello descriptions render markdown.
- Never put a token, an API key, a password or any other credential on a card: a card is as
  public as the board.

## Steps

1. Auth lives in `~/.trello-cli/default/config.json` (`apiKey`, `token`). If a command reports a
   missing key or token, the values are under `## Trello CLI` in `~/.secrets/thinktech/secrets.md`
   (one Trello account serves every board); set them with `trello auth:api-key <key>` and
   `trello auth:token <token>`, never echo them.
2. If any command says `no such table: boards` or `List [...] not found on board`, run
   `trello sync` once (it caches boards and lists locally, and the cache goes stale when a list
   is added) and retry.
3. Create the card. Pass the description through a file to avoid shell quoting problems:
   ```
   trello card:create --board "AppShapes" --list "Cavalry" --name "<title>" \
       --description "$(sed '1{/^# /d;}' card.md)" --format json
   ```
   Optional flags: `--label <name>` (repeatable), `--due "<date or natural language>"`,
   `--position top|bottom`.
4. Read the returned JSON and report the card number and URL: the number is the path segment
   before the slug in `url` (`/c/<short>/<number>-<slug>`); `id` is the long id.
5. To attach a link afterwards: `trello card:attach --board ... --list ... --card "<title>" --url <url>`.
   To add a checklist: `trello card:checklist ... --name "<name>"`.

## Report

One line per card: number, title, list, URL. Do not paste the description back.
