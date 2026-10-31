#!/usr/bin/env python3
"""Mark a Trello card complete and archive it: one PUT on the card.

usage: python3 -I close.py <card id>

The trello-cli credentials are read from ~/.trello-cli/default/config.json
and sent in the Authorization header: never on argv, never printed.
"""
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit("usage: close.py <card id>")
    card = sys.argv[1].strip()
    if len(card) != 24 or any(c not in "0123456789abcdef" for c in card):
        sys.exit("close.py: the argument must be the card's 24-character id")
    with open(os.path.expanduser("~/.trello-cli/default/config.json"), encoding="utf-8") as f:
        cfg = json.load(f)
    key, token = cfg["apiKey"], cfg["token"]
    body = urllib.parse.urlencode({"dueComplete": "true", "closed": "true"}).encode()
    req = urllib.request.Request(
        f"https://api.trello.com/1/cards/{card}",
        data=body,
        method="PUT",
        headers={
            "Authorization": f'OAuth oauth_consumer_key="{key}", oauth_token="{token}"',
            "Content-Type": "application/x-www-form-urlencoded",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            d = json.load(r)
    except urllib.error.HTTPError as e:
        sys.exit(f"close.py: Trello answered {e.code} {e.reason}")
    print(f"closed: {d.get('name')} | complete={d.get('dueComplete')} archived={d.get('closed')}")


if __name__ == "__main__":
    main()
