"""Wrap Japanese UI strings in web/*.js with t() and list the keys to translate.

The UI is written in Japanese; English comes from web/i18n-en.js (built from en.json by
build.py). This script is idempotent: strings already wrapped in t(...) are left alone.

    python docs/i18n/transform.py          # rewrite web/*.js in place, write keys.json
    python docs/i18n/transform.py --check  # only report what would change
"""
import io
import json
import os
import re
import sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
WEB = os.path.join(ROOT, "web")
FILES = ["app.js", "workbench.js", "fx.js", "code.js", "palette.js", "chat.js", "team.js", "editors.js", "search.js", "sidebar.js"]
JP_WORD = re.compile(r"[぀-ヿ一-鿿]")  # kana or CJK ideograph: real words, not just punctuation
REGEX_PREV = set("(,=:[!&|?{};+-*%<>~^")
KEYWORDS_BEFORE_REGEX = {"return", "typeof", "case", "in", "of", "do", "else", "void", "delete", "throw", "new"}

keys = {}


def note(key, where):
    keys.setdefault(key, set()).add(where)


def decode_js(s):
    out = []
    i = 0
    while i < len(s):
        c = s[i]
        if c == "\\" and i + 1 < len(s):
            n = s[i + 1]
            if n == "n":
                out.append("\n"); i += 2; continue
            if n == "t":
                out.append("\t"); i += 2; continue
            if n == "u" and i + 5 < len(s):
                out.append(chr(int(s[i + 2:i + 6], 16))); i += 6; continue
            out.append(n); i += 2; continue
        out.append(c)
        i += 1
    return "".join(out)


def js_string(s):
    return json.dumps(s, ensure_ascii=False)


class Rewriter:
    def __init__(self, src, name):
        self.src = src
        self.name = name
        self.i = 0
        self.out = []
        self.changed = 0

    def prev_significant(self):
        j = len(self.out) - 1
        buf = "".join(self.out[-200:])
        k = len(buf) - 1
        while k >= 0 and buf[k] in " \t\r\n":
            k -= 1
        if k < 0:
            return "", ""
        word = re.search(r"([A-Za-z_$][\w$]*)$", buf[: k + 1])
        return buf[k], (word.group(1) if word else "")

    def run(self):
        s = self.src
        n = len(s)
        while self.i < n:
            c = s[self.i]
            two = s[self.i : self.i + 2]
            if two == "//":
                j = s.find("\n", self.i)
                j = n if j < 0 else j
                self.out.append(s[self.i : j]); self.i = j; continue
            if two == "/*":
                j = s.find("*/", self.i)
                j = n if j < 0 else j + 2
                self.out.append(s[self.i : j]); self.i = j; continue
            if c in "'\"":
                self.string(c); continue
            if c == "`":
                self.template(); continue
            if c == "/":
                prev, word = self.prev_significant()
                if prev == "" or prev in REGEX_PREV or word in KEYWORDS_BEFORE_REGEX:
                    self.regex(); continue
            self.out.append(c)
            self.i += 1
        return "".join(self.out)

    def string(self, q):
        s = self.src
        j = self.i + 1
        while j < len(s) and s[j] != q:
            if s[j] == "\\":
                j += 1
            j += 1
        lit = s[self.i : j + 1]
        self.i = j + 1
        body = lit[1:-1]
        if JP_WORD.search(body):
            key = decode_js(body)
            note(key, self.name)
            if self.already_wrapped():
                self.out.append(lit)
            else:
                self.out.append("t(" + lit + ")")
                self.changed += 1
        else:
            self.out.append(lit)

    def already_wrapped(self):
        buf = "".join(self.out[-10:])
        return buf.rstrip().endswith("t(")

    def regex(self):
        s = self.src
        j = self.i + 1
        in_class = False
        while j < len(s):
            ch = s[j]
            if ch == "\\":
                j += 2; continue
            if ch == "[":
                in_class = True
            elif ch == "]":
                in_class = False
            elif ch == "/" and not in_class:
                break
            elif ch == "\n":
                break
            j += 1
        j += 1
        while j < len(s) and s[j].isalpha():
            j += 1
        self.out.append(s[self.i : j])
        self.i = j

    def template(self):
        """Copy a template literal, wrapping Japanese runs in its static parts and recursing
        into ${...} expressions."""
        s = self.src
        self.out.append("`")
        self.i += 1
        chunk = []
        while self.i < len(s):
            c = s[self.i]
            if c == "\\":
                chunk.append(s[self.i : self.i + 2]); self.i += 2; continue
            if c == "`":
                self.out.append(self.wrap_runs("".join(chunk)))
                self.out.append("`")
                self.i += 1
                return
            if s.startswith("${", self.i):
                self.out.append(self.wrap_runs("".join(chunk)))
                chunk = []
                self.out.append("${")
                self.i += 2
                self.expression()
                continue
            chunk.append(c)
            self.i += 1
        self.out.append("".join(chunk))

    def expression(self):
        """Inside ${ ... }: tokenize until the matching }."""
        s = self.src
        depth = 1
        while self.i < len(s):
            c = s[self.i]
            if c in "'\"":
                self.string(c); continue
            if c == "`":
                self.template(); continue
            if s.startswith("//", self.i) or s.startswith("/*", self.i):
                self.out.append(c); self.i += 1; continue
            if c == "/":
                prev, word = self.prev_significant()
                if prev in REGEX_PREV or word in KEYWORDS_BEFORE_REGEX:
                    self.regex(); continue
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    self.out.append("}")
                    self.i += 1
                    return
            self.out.append(c)
            self.i += 1

    def wrap_runs(self, text):
        if not JP_WORD.search(text) or self.already_wrapped_template(text):
            return text
        # A run: text between HTML/template delimiters that contains a Japanese word.
        pattern = re.compile(r"[^<>\"`{}\n]*" + JP_WORD.pattern + r"[^<>\"`{}\n]*")
        out = []
        pos = 0
        for m in pattern.finditer(text):
            run = m.group(0)
            lead = len(run) - len(run.lstrip())
            trail = len(run) - len(run.rstrip())
            core = run.strip()
            if not core or not JP_WORD.search(core):
                continue
            start = m.start() + lead
            end = m.end() - trail
            out.append(text[pos:start])
            key = decode_js(core)
            note(key, self.name)
            out.append("${t(" + js_string(key) + ")}")
            self.changed += 1
            pos = end
        out.append(text[pos:])
        return "".join(out)

    def already_wrapped_template(self, text):
        return False


def html_keys():
    path = os.path.join(WEB, "index.html")
    src = io.open(path, encoding="utf-8").read()
    body = src[src.find("<body") :]
    for m in re.finditer(r">([^<>]+)<", body):
        t = m.group(1).strip()
        if t and JP_WORD.search(t):
            note(t, "index.html")
    for m in re.finditer(r'(?:title|placeholder|alt|aria-label)="([^"]+)"', body):
        t = m.group(1).strip()
        if t and JP_WORD.search(t):
            note(t, "index.html")


def main():
    check = "--check" in sys.argv
    total = 0
    for name in FILES:
        path = os.path.join(WEB, name)
        src = io.open(path, encoding="utf-8", newline="").read()
        rw = Rewriter(src, name)
        out = rw.run()
        total += rw.changed
        if rw.changed and not check:
            io.open(path, "w", encoding="utf-8", newline="").write(out)
        print(f"{name}: {rw.changed} strings")
    html_keys()
    here = os.path.dirname(os.path.abspath(__file__))
    data = {k: sorted(v) for k, v in sorted(keys.items())}
    if not check:
        io.open(os.path.join(here, "keys.json"), "w", encoding="utf-8", newline="\n").write(json.dumps(data, ensure_ascii=False, indent=1) + "\n")
    print(f"{total} strings wrapped, {len(keys)} unique keys")


if __name__ == "__main__":
    main()
