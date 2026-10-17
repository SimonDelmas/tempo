"""Accès au vault : lecture/écriture des fichiers Markdown et du frontmatter."""

import datetime as dt
import hashlib
import re
import shutil
import unicodedata
from pathlib import Path

import yaml

IGNORED_DIRS = {".git", ".tempo", ".obsidian", ".trash", "node_modules"}
FRONTMATTER_RE = re.compile(r"\A---\r?\n(.*?)\r?\n---[ \t]*(?:\r?\n|\Z)", re.S)


class VaultError(Exception):
    pass


class NotFoundError(VaultError):
    pass


class ConflictError(VaultError):
    """Le fichier a été modifié ailleurs depuis la dernière lecture."""

    def __init__(self, current_content: str):
        super().__init__("Le fichier a été modifié par ailleurs")
        self.current_content = current_content


def content_hash(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


def slugify(text: str, max_len: int = 50) -> str:
    ascii_text = unicodedata.normalize("NFKD", text).encode("ascii", "ignore").decode()
    slug = re.sub(r"[^a-zA-Z0-9]+", "-", ascii_text).strip("-").lower()
    return slug[:max_len].strip("-") or "note"


def split_frontmatter(text: str) -> tuple[dict, str]:
    match = FRONTMATTER_RE.match(text)
    if not match:
        return {}, text
    try:
        meta = yaml.safe_load(match.group(1)) or {}
    except yaml.YAMLError:
        return {}, text
    if not isinstance(meta, dict):
        return {}, text
    return meta, text[match.end():]


class _FrontmatterDumper(yaml.SafeDumper):
    """Clés en style bloc, listes de valeurs simples en ligne : `tags: [a, b]`."""


def _represent_list(dumper: yaml.SafeDumper, data: list):
    flow = all(not isinstance(item, (dict, list)) for item in data)
    return dumper.represent_sequence("tag:yaml.org,2002:seq", data, flow_style=flow)


_FrontmatterDumper.add_representer(list, _represent_list)


def join_frontmatter(meta: dict, body: str) -> str:
    if not meta:
        return body
    dumped = yaml.dump(meta, Dumper=_FrontmatterDumper, sort_keys=False, allow_unicode=True,
                       default_flow_style=False, width=1000).strip()
    return f"---\n{dumped}\n---\n{body}"


def jsonable(value):
    """Convertit les dates YAML en chaînes ISO pour l'index et l'API."""
    if isinstance(value, (dt.date, dt.datetime)):
        return value.isoformat()
    if isinstance(value, dict):
        return {str(k): jsonable(v) for k, v in value.items()}
    if isinstance(value, list):
        return [jsonable(v) for v in value]
    return value


def append_to_section(text: str, heading: str, line: str) -> str:
    """Ajoute `line` à la fin de la section `heading` (créée si absente)."""
    lines = text.rstrip("\n").split("\n")
    try:
        start = next(i for i, l in enumerate(lines) if l.strip() == heading)
    except StopIteration:
        return text.rstrip("\n") + f"\n\n{heading}\n{line}\n"
    level = len(heading) - len(heading.lstrip("#"))
    end = len(lines)
    for i in range(start + 1, len(lines)):
        m = re.match(r"^(#{1,6})\s", lines[i])
        if m and len(m.group(1)) <= level:
            end = i
            break
    insert_at = end
    while insert_at - 1 > start and not lines[insert_at - 1].strip():
        insert_at -= 1
    lines.insert(insert_at, line)
    if insert_at + 1 < len(lines) and lines[insert_at + 1].strip():
        lines.insert(insert_at + 1, "")
    return "\n".join(lines) + "\n"


def section_lines(text: str, heading: str) -> list[str]:
    lines = text.split("\n")
    try:
        start = next(i for i, l in enumerate(lines) if l.strip() == heading)
    except StopIteration:
        return []
    level = len(heading) - len(heading.lstrip("#"))
    out = []
    for l in lines[start + 1:]:
        m = re.match(r"^(#{1,6})\s", l)
        if m and len(m.group(1)) <= level:
            break
        out.append(l)
    return out


def strip_section(text: str, heading: str) -> str:
    """Retire une section (ex. le texte original d'un échange) pour alléger un contexte."""
    lines = text.split("\n")
    try:
        start = next(i for i, l in enumerate(lines) if l.strip() == heading)
    except StopIteration:
        return text
    kept = section_lines(text, heading)
    return "\n".join(lines[:start] + lines[start + 1 + len(kept):])


class Vault:
    def __init__(self, root: Path):
        self.root = Path(root).resolve()
        self.root.mkdir(parents=True, exist_ok=True)

    def resolve(self, rel: str) -> Path:
        rel = rel.replace("\\", "/").lstrip("/")
        path = (self.root / rel).resolve()
        if path != self.root and self.root not in path.parents:
            raise VaultError("Chemin hors du vault")
        if path.suffix.lower() != ".md":
            raise VaultError("Seuls les fichiers .md sont gérés")
        return path

    def rel(self, path: Path) -> str:
        return Path(path).resolve().relative_to(self.root).as_posix()

    def is_ignored(self, path: Path) -> bool:
        try:
            parts = Path(path).resolve().relative_to(self.root).parts
        except ValueError:
            return True
        return any(p in IGNORED_DIRS or p.startswith(".") for p in parts[:-1])

    def list_files(self) -> list[str]:
        return sorted(
            self.rel(p) for p in self.root.rglob("*.md") if p.is_file() and not self.is_ignored(p)
        )

    def exists(self, rel: str) -> bool:
        return self.resolve(rel).is_file()

    def read_text(self, rel: str) -> str:
        path = self.resolve(rel)
        if not path.is_file():
            raise NotFoundError(f"Note introuvable : {rel}")
        return path.read_text(encoding="utf-8")

    def read(self, rel: str) -> dict:
        text = self.read_text(rel)
        meta, _ = split_frontmatter(text)
        return {"path": rel, "content": text, "hash": content_hash(text), "meta": jsonable(meta)}

    def write(self, rel: str, content: str, expected_hash: str | None = None,
              create_only: bool = False) -> str:
        path = self.resolve(rel)
        if path.exists():
            if create_only:
                raise VaultError(f"Le fichier existe déjà : {rel}")
            current = path.read_text(encoding="utf-8")
            if expected_hash is not None and content_hash(current) != expected_hash:
                raise ConflictError(current)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8", newline="")
        return content_hash(content)

    def delete(self, rel: str) -> None:
        """Déplace le fichier dans .trash/ (récupérable)."""
        path = self.resolve(rel)
        if not path.is_file():
            raise NotFoundError(f"Note introuvable : {rel}")
        stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
        target = self.root / ".trash" / stamp / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(path), str(target))

    def unique_path(self, folder: str, stem: str) -> str:
        folder = folder.strip("/")
        candidate = f"{folder}/{stem}.md" if folder else f"{stem}.md"
        n = 2
        while self.resolve(candidate).exists():
            candidate = f"{folder}/{stem}-{n}.md" if folder else f"{stem}-{n}.md"
            n += 1
        return candidate
