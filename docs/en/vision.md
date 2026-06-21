# Vision

Lumi is an open-source app for deliberate reading and learning over materials
the user already chose: books, articles, threads, messages and notes.

At first glance, Lumi may look similar to Readwise, but the goal is broader. It
is not only a reader and not only a highlight manager. It is a tool that helps
turn reading into knowledge: save material, understand it, return to important
ideas and connect what was read to a personal knowledge base.

## Core Idea

Reading is usually scattered across many places: books in one app, articles in
the browser, notes in Obsidian, discussions in Telegram or X. Useful ideas get
lost, and review or reflection depends on personal discipline.

Lumi should collect this process into one working loop:

- add material from different sources;
- read in a comfortable format;
- create notes, highlights and margin notes;
- return to previous material through questions and review;
- use AI where it genuinely helps: explain a fragment, summarize, prepare
  cards, connect ideas;
- add a social layer that increases engagement and broadens the user's
  perspective.

## Key Product Directions

### Formats

Lumi should support several material types:

- book formats: EPUB, FB2, PDF;
- reader-mode web pages; popular sites such as Medium, Substack and Habr are
  validation targets and fixtures, not separate site-specific implementations;
- Telegram through a bot;
- X: long posts and threads, with the integration format still to be decided;
- Markdown;
- the custom `lum` format.

`lum` is a Markdown-based book format. The idea is close to Obsidian's approach:
a directory with Markdown files, local assets and metadata opens as one book.
However, `lum` is not an arbitrary notes vault: reading order, table of
contents, cover, assets and capabilities are defined by a manifest/spine, and
inside the reader the material behaves as a read-only paginated document. The
folder can be packaged into one portable `.lum` package for import, sync and
sharing. This format can also become interactive: diagrams, visualizations,
embedded tasks and other elements that do not fit well into a static book.

### Notes and Reading

Reading notes are central to the product. The toolset should be rich but not
overloaded:

- regular notes attached to a selected fragment;
- different highlight types, such as color or bold emphasis;
- margin notes, not only comments attached to selected text;
- voice notes to capture thoughts with minimal friction;
- future voice-note transcription;
- Obsidian-style links inside notes;
- Obsidian integration;
- search and RAG over notes. For the first stage, BM25 + fastText is enough;
  later this layer can be replaced or strengthened.

### Sync

User data should be available on the user's clients as a complete state copy,
not only as a remote server database. In this model, the backend is primarily a
connection point: sync clients, support collaborative reading, store data passed
between clients and serve functions that require a server.

This does not mean data must exist only in a local filesystem on a single
device. For web and collaboration scenarios, the server can store state and
files, but the user must have full access to the contents: open, download, save
and export their materials.

For the first web version, Lumi accepts a classic cloud-backed architecture:
materials, files, indexes and web-client state are stored in a cloud account
replica, while browser storage is only an auxiliary cache. Full local-first /
full-copy replicas are mainly future desktop and mobile client work.

In the long term, Lumi should give the user a path to maximum privacy: after
mature native clients exist, the user can disable the web/cloud replica and keep
their personal library, notes, knowledge base, indexes and blobs only on their
own devices. The server then remains for account/device bootstrap, encrypted
sync relay/key envelopes, social/shared coordination and explicitly published
objects. The raw seed phrase is never stored in the cloud.

### Web Account

Because the first Lumi version is web, it needs a real user account and cloud
data copy. This is a specific exception to the broader local-storage idea: a
web client cannot reliably depend only on a local user folder, so materials,
files and state live in the account cloud replica.

Registration should be as simple as a crypto wallet: the user receives a seed
phrase that acts as the main credential for login and recovery. The stable user
id should be UUIDv7 or a newer time-ordered UUID version. A nickname can be
added separately, but only as a social signature and display name, not as a
login.

The web account is the center of the first web version, but it must not become
the unavoidable center of the whole architecture. It is needed as the first
client, cloud replica, entry point for Telegram/import flows and bootstrap path
for other clients. Desktop and mobile should still receive full data copies via
sync and later support private mode without a cloud copy of the personal
library.

### Learning After Reading

Reading should not end on the last page. Lumi should help the user return to
material and check what actually stuck.

Base scenarios:

- quiz over completed material after reading;
- text or voice answers;
- questions over previously read material with revealable hints;
- spaced review based on the forgetting curve, with an option to turn it off;
- an exercise where the user explains the material in their own words and AI
  provides feedback.

### Social Features

The social layer is not a feed for its own sake. It is a way to read and discuss
texts together.

Possible features:

- collaborative reading with a shared comment area;
- visible participant highlights;
- sharing read books and materials;
- recommendations based on what the user has read and saved.

### AI

The AI layer in Lumi should be replaceable and controlled by the user. The user
can add their own model API key or use an in-app subscription.

Core scenarios:

- summaries and notes;
- explanation of difficult fragments;
- card and question generation from read material;
- building a personal knowledge base from summaries, notes and highlights;
- extracting key entities and relationships for a future knowledge graph.

A separate direction is agent integration through MCP. Every AI-requiring
function can be modeled as a task queue. The app creates tasks, and a connected
agent performs them: prepare summaries, create cards, reshape notes and fill in
missing data.

### Extensibility

In the future, Lumi should consider a plugin system. It would allow new sources,
formats, processing methods and integrations to be added without changing the
core app.

## Screen Structure

- Library: a document work view, possibly with folders, and definitely with
  search, tags, sorting and filtering.
- Reading screen: format-independent reading, including optional learning
  mechanics embedded into the reading flow.
- Unified search over notes and materials.
- Challenges: tests over completed material and skipped reading tasks.
- Knowledge base: close to an Obsidian vault, but not a full Obsidian clone.

## Platforms

The first focus is Web. It is the fastest way to validate product hypotheses and
gather an early audience. For the web version, Lumi stores files and state in a
cloud account replica, but the user must have export and the ability to obtain
full copies on future desktop/mobile clients.

Later candidates:

- Mobile, starting with Android;
- Desktop.

## Technologies

Base stack:

- Rust;
- Dioxus Fullstack;
- Axum;
- SQLx.
