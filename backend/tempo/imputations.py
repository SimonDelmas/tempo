"""Comptes d'imputation et saisie des heures (8 h = 1 jour) par semaine.

- un compte = un fichier comptes/<alias>.md (nom, alias, jours attribués) ;
- un projet est rattaché à un compte par `compte: <alias>` dans son frontmatter ;
- les heures d'une semaine sont dans imputations/AAAA/AAAA-Sxx.md (frontmatter `hours`,
  tableau régénéré dans le corps) ;
- l'IA peut proposer la répartition d'une semaine à partir du rapport hebdomadaire.
"""

from __future__ import annotations

import datetime as dt
import re

from pydantic import BaseModel

from .config import Settings
from .index import Index
from .llm import LLM
from .prompts import IMPUTATION_SYSTEM
from .reports import ReportService, week_bounds
from .vault import (NotFoundError, Vault, VaultError, join_frontmatter, slugify,
                    split_frontmatter)

HOURS_PER_DAY = 8
STEP = 0.5  # granularité des heures saisies
ALIAS_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,39}$")
DAY_NAMES = ["Lun", "Mar", "Mer", "Jeu", "Ven", "Sam", "Dim"]


def week_label(day: dt.date) -> str:
    year, week, _ = day.isocalendar()
    return f"{year}-S{week:02d}"


def workdays(start: dt.date) -> list[str]:
    return [(start + dt.timedelta(days=i)).isoformat() for i in range(5)]


def _round(hours: float) -> float:
    value = round(round(float(hours) / STEP) * STEP, 2)
    return int(value) if value.is_integer() else value  # `8` plutôt que `8.0` dans le YAML


def _number(value, field: str) -> float:
    try:
        number = float(value)
    except (TypeError, ValueError) as exc:
        raise VaultError(f"Valeur invalide pour {field} : {value}") from exc
    if number < 0:
        raise VaultError(f"{field} ne peut pas être négatif")
    return number


def _fmt(hours: float) -> str:
    return f"{hours:g}"


def _clean_hours(raw) -> dict[str, dict[str, float]]:
    """Frontmatter `hours` → {date: {alias: heures}} sans les valeurs nulles."""
    result: dict[str, dict[str, float]] = {}
    if not isinstance(raw, dict):
        return result
    for day, entries in raw.items():
        if not isinstance(entries, dict):
            continue
        kept = {}
        for alias, hours in entries.items():
            try:
                value = float(hours)
            except (TypeError, ValueError):
                continue
            if value > 0:
                kept[str(alias)] = value
        if kept:
            result[str(day)[:10]] = kept
    return result


def balance_day(entries: dict[str, float]) -> dict[str, float]:
    """Arrondit au pas de saisie et ramène le total de la journée à 8 h."""
    entries = {a: h for a, h in entries.items() if h > 0}
    total = sum(entries.values())
    if not total:
        return {}
    scaled = {a: _round(h * HOURS_PER_DAY / total) for a, h in entries.items()}
    gap = round(HOURS_PER_DAY - sum(scaled.values()), 2)
    if gap:
        biggest = max(scaled, key=lambda a: scaled[a])
        scaled[biggest] = round(scaled[biggest] + gap, 2)
    return {a: h for a, h in scaled.items() if h > 0}


class ProposalEntry(BaseModel):
    compte: str
    hours: float


class ProposalDay(BaseModel):
    date: str
    entries: list[ProposalEntry] = []


class ImputationProposal(BaseModel):
    days: list[ProposalDay] = []
    comment: str = ""


class ImputationService:
    accounts_folder = "comptes"
    folder = "imputations"

    def __init__(self, vault: Vault, index: Index, reports: ReportService, llm: LLM,
                 settings: Settings):
        self.vault = vault
        self.index = index
        self.reports = reports
        self.llm = llm
        self.settings = settings

    # --- comptes ------------------------------------------------------------

    def _account_notes(self) -> list[dict]:
        return [n for n in self.index.list_notes(type="compte") if n["alias"]]

    def _account_note(self, alias: str) -> dict:
        for note in self._account_notes():
            if note["alias"].lower() == alias.strip().lower():
                return note
        raise NotFoundError(f"Compte d'imputation introuvable : {alias}")

    def consumed_hours(self, exclude_week: str | None = None) -> dict[str, float]:
        totals: dict[str, float] = {}
        for note in self.index.list_notes(type="imputation"):
            if exclude_week and note["meta"].get("week") == exclude_week:
                continue
            for entries in _clean_hours(note["meta"].get("hours")).values():
                for alias, hours in entries.items():
                    totals[alias.lower()] = totals.get(alias.lower(), 0) + hours
        return totals

    def accounts(self) -> list[dict]:
        consumed = self.consumed_hours()
        projects = self.projects()
        result = []
        for note in self._account_notes():
            alias = note["alias"]
            days = float(note["meta"].get("days") or 0)
            used = consumed.get(alias.lower(), 0) / HOURS_PER_DAY
            result.append({
                "alias": alias, "title": note["title"], "days": days,
                "consumed_days": round(used, 3), "remaining_days": round(days - used, 3),
                "projects": [p["id"] for p in projects
                             if (p["compte"] or "").lower() == alias.lower()],
                "path": note["path"],
            })
        return sorted(result, key=lambda a: a["alias"].lower())

    def create_account(self, title: str, alias: str, days) -> dict:
        title, alias = title.strip(), alias.strip()
        if not title:
            raise VaultError("Le nom du compte est obligatoire")
        if not ALIAS_RE.match(alias):
            raise VaultError("Alias invalide : lettres, chiffres, « - », « _ » ou « . », sans espace")
        if self.index.resolve_link(alias):
            raise VaultError(f"L'alias « {alias} » est déjà utilisé")
        meta = {"type": "compte", "title": title, "alias": alias, "days": _number(days, "days")}
        body = f"\n# {title}\n\nCompte d'imputation `{alias}`.\n"
        rel = self.vault.unique_path(self.accounts_folder, slugify(alias))
        self.vault.write(rel, join_frontmatter(meta, body), create_only=True)
        self.index.upsert(rel)
        return next(a for a in self.accounts() if a["alias"] == alias)

    def update_account(self, alias: str, title: str | None = None, days=None) -> dict:
        note = self._account_note(alias)
        meta, body = split_frontmatter(self.vault.read_text(note["path"]))
        if title is not None:
            if not title.strip():
                raise VaultError("Le nom du compte est obligatoire")
            meta["title"] = title.strip()
        if days is not None:
            meta["days"] = _number(days, "days")
        self.vault.write(note["path"], join_frontmatter(meta, body))
        self.index.upsert(note["path"])
        return next(a for a in self.accounts() if a["alias"] == note["alias"])

    # --- projets ------------------------------------------------------------

    def projects(self) -> list[dict]:
        return sorted(({"id": n["stem"], "title": n["title"], "path": n["path"],
                        "compte": n["meta"].get("compte") or None}
                       for n in self.index.list_notes(type="projet")),
                      key=lambda p: p["title"].lower())

    def set_project_account(self, project_id: str, alias: str | None) -> dict:
        project = next((p for p in self.projects() if p["id"] == project_id), None)
        if not project:
            raise NotFoundError(f"Projet introuvable : {project_id}")
        meta, body = split_frontmatter(self.vault.read_text(project["path"]))
        if alias:
            meta["compte"] = self._account_note(alias)["alias"]
        else:
            meta.pop("compte", None)
        self.vault.write(project["path"], join_frontmatter(meta, body))
        self.index.upsert(project["path"])
        return next(p for p in self.projects() if p["id"] == project_id)

    # --- semaines -----------------------------------------------------------

    def week_path(self, day: dt.date) -> str:
        start, _ = week_bounds(day)
        return f"{self.folder}/{start.year}/{week_label(start)}.md"

    def week(self, day: dt.date) -> dict:
        start, end = week_bounds(day)
        rel = self.week_path(day)
        meta, comment = {}, ""
        if self.vault.exists(rel):
            meta, _ = split_frontmatter(self.vault.read_text(rel))
            comment = str(meta.get("comment") or "")
        hours = _clean_hours(meta.get("hours"))
        days = workdays(start)
        totals = {d: round(sum(hours.get(d, {}).values()), 2) for d in days}
        return {"week": week_label(start), "start": start.isoformat(), "end": end.isoformat(),
                "days": days, "hours": hours, "totals": totals, "comment": comment,
                "path": rel if self.vault.exists(rel) else None}

    def incomplete_days(self, day: dt.date) -> list[str]:
        """Jours ouvrés de la semaine, jusqu'à `day` inclus, dont le total n'est pas 8 h."""
        week = self.week(day)
        return [d for d in week["days"] if d <= day.isoformat()
                and week["totals"][d] != HOURS_PER_DAY]

    def save_week(self, day: dt.date, hours: dict, comment: str = "") -> dict:
        start, end = week_bounds(day)
        known = {a["alias"].lower(): a["alias"] for a in self.accounts()}
        clean: dict[str, dict[str, float]] = {}
        for date, entries in (hours or {}).items():
            date_obj = dt.date.fromisoformat(str(date)[:10])
            if not start <= date_obj <= end:
                raise VaultError(f"{date} n'appartient pas à la semaine {week_label(start)}")
            kept = {}
            for alias, value in (entries or {}).items():
                number = _number(value, f"heures {alias} le {date}")
                if not number:
                    continue
                if alias.lower() not in known:
                    raise NotFoundError(f"Compte d'imputation introuvable : {alias}")
                kept[known[alias.lower()]] = _round(number)
            if sum(kept.values()) > 24:
                raise VaultError(f"Plus de 24 h saisies le {date}")
            if kept:
                clean[date_obj.isoformat()] = kept
        rel = self.week_path(day)
        meta = {"type": "imputation", "week": week_label(start), "period_start": start,
                "period_end": end, "hours": {d: clean[d] for d in sorted(clean)}}
        if comment.strip():
            meta["comment"] = comment.strip()
        self.vault.write(rel, join_frontmatter(meta, self._render(start, clean, comment)))
        self.index.upsert(rel)
        return self.week(day)

    @staticmethod
    def _render(start: dt.date, hours: dict[str, dict[str, float]], comment: str) -> str:
        days = sorted(set(workdays(start)) | set(hours))
        aliases = sorted({a for entries in hours.values() for a in entries}, key=str.lower)
        header = [DAY_NAMES[dt.date.fromisoformat(d).weekday()] + " " + d[8:10] + "/" + d[5:7]
                  for d in days]
        lines = [f"\n# Imputations {week_label(start)}\n",
                 "| Compte | " + " | ".join(header) + " | Total |",
                 "|---|" + "---:|" * (len(days) + 1)]
        for alias in aliases:
            values = [hours.get(d, {}).get(alias, 0) for d in days]
            lines.append(f"| [[{alias}]] | " + " | ".join(_fmt(v) if v else "" for v in values)
                         + f" | {_fmt(sum(values))} |")
        totals = [sum(hours.get(d, {}).values()) for d in days]
        lines.append("| **Total** | " + " | ".join(_fmt(t) for t in totals)
                     + f" | {_fmt(sum(totals))} |")
        text = "\n".join(lines) + "\n"
        if comment.strip():
            text += f"\n## Justification\n\n{comment.strip()}\n"
        return text

    # --- proposition IA -----------------------------------------------------

    def _weekly_report_text(self, start: dt.date) -> str | None:
        label = week_label(start)
        reports = [n for n in self.index.list_notes(type="rapport")
                   if n["meta"].get("kind") == "hebdo" and n["meta"].get("week") == label]
        if not reports:
            return None
        latest = max(reports, key=lambda n: str(n["meta"].get("generated") or ""))
        _, body = split_frontmatter(self.vault.read_text(latest["path"]))
        return body.strip()

    def proposal_context(self, day: dt.date) -> str:
        start, end = week_bounds(day)
        label = week_label(start)
        consumed = self.consumed_hours(exclude_week=label)
        accounts = self.accounts()
        lines = [f"Semaine {label} du {start} au {end}. Jours à répartir : "
                 + ", ".join(f"{DAY_NAMES[i]} {d}" for i, d in enumerate(workdays(start))) + "."]
        lines.append("\n## Comptes d'imputation\n" + "\n".join(
            f"- {a['alias']} — {a['title']} | jours attribués : {_fmt(a['days'])} | "
            f"consommés hors cette semaine : {_fmt(round(consumed.get(a['alias'].lower(), 0) / HOURS_PER_DAY, 2))}"
            f" | restants avant cette semaine : "
            f"{_fmt(round(a['days'] - consumed.get(a['alias'].lower(), 0) / HOURS_PER_DAY, 2))}"
            f" | projets : {', '.join(a['projects']) or '-'}"
            for a in accounts))
        linked = [p for p in self.projects() if p["compte"]]
        if linked:
            lines.append("\n## Projets rattachés à un compte\n" + "\n".join(
                f"- {p['id']} ({p['title']}) → {p['compte']}" for p in linked))
        current = self.week(day)
        if current["hours"]:
            lines.append("\n## Heures déjà saisies cette semaine (à respecter)\n" + "\n".join(
                f"- {d} : " + ", ".join(f"{a} {_fmt(h)} h" for a, h in entries.items())
                for d, entries in sorted(current["hours"].items())))
        report = self._weekly_report_text(start)
        if report:
            lines.append("\n## Rapport hebdomadaire de la semaine\n" + report)
        else:
            lines.append("\n## Activité de la semaine (pas de rapport hebdomadaire généré)\n"
                         + self.reports.weekly_context(start, end))
        return "\n".join(lines)

    def propose(self, day: dt.date) -> dict:
        start, _ = week_bounds(day)
        accounts = self.accounts()
        if not accounts:
            raise VaultError("Aucun compte d'imputation : créez-en au moins un")
        known = {a["alias"].lower(): a["alias"] for a in accounts}
        proposal = self.llm.chat_json(
            [{"role": "system", "content": IMPUTATION_SYSTEM},
             {"role": "user", "content": self.proposal_context(day)}],
            model=self.settings.model_smart, schema=ImputationProposal)
        days = set(workdays(start))
        hours: dict[str, dict[str, float]] = {}
        for item in proposal.days:
            date = str(item.date)[:10]
            if date not in days:
                continue
            entries: dict[str, float] = {}
            for entry in item.entries:
                alias = known.get(entry.compte.strip().lower())
                if alias and entry.hours > 0:
                    entries[alias] = entries.get(alias, 0) + entry.hours
            balanced = balance_day(entries)
            if balanced:
                hours[date] = balanced
        return {"hours": hours, "comment": proposal.comment.strip()}
