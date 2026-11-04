# Concepts

## Items and revisions

An **item** is a part, assembly, document or software component with a
unique number – `PRT-000042`, `ASM-…`, `DOC-…`, `SW-…`, or a number of your
own scheme. Its state of development is kept in **revisions** `A, B, …, Y,
AA, …` (I, O, Q, S, X and Z are skipped, as in ASME Y14.35). A revision
carries the attributes (name, description, material, mass, unit cost), the
files and the bill of materials.

## Lifecycle

```text
 WIP ──submit──▶ In Review ──all reviewers approve──▶ Released ──▶ Obsolete
  ▲                 │
  └──reject/withdraw┘
```

| Transition           | How                                                                               | Who                                                                      |
|----------------------|-----------------------------------------------------------------------------------|--------------------------------------------------------------------------|
| WIP → In Review      | submit a change order containing the revision                                     | the change's author (CAD editor of the item's project group) or an admin |
| In Review → Released | automatically when every reviewer approved – all revisions of the change together | the assigned reviewers, never the author (four-eyes principle)           |
| In Review → WIP      | a reviewer rejects with a reason, or the author withdraws                         | reviewers; author or admin                                               |
| Released → Obsolete  | *Mark obsolete* (final)                                                           | CAD editors of the project group, admins                                 |

Only WIP revisions change, and only while they are checked out. Released
data never changes – further work happens in a new revision (*Revise*).

## Project groups and roles

| Role       | May                                                                                                 |
|------------|-----------------------------------------------------------------------------------------------------|
| Viewer     | read everything                                                                                     |
| CAD editor | create and change items of the project groups they belong to, create project groups, review changes |
| Admin      | everything, plus users, all project groups and lock overrides                                       |

Every item belongs to a **project group** (a rocket, a rover, a product
line). CAD editors start their own groups and become their **managers**:
managers edit the group and add or remove members. A group can also be bound
to a group of the identity provider (administrators only); its members are
then synchronised at every single sign-on.

## Audit log

Every modification is recorded with user, time and details – shown on every
revision page and, for admins, under *Audit log*.
