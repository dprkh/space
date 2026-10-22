# Space

A small Rust agent harness with a [GPUI](https://gpui.rs/) interface. It uses Google's `gemini-4-argon` model and works inside one project folder.

## Build and launch

Use macOS with Xcode and Rust installed. The repository selects Rust 1.98.1. GPUI compiles its Metal shaders at runtime, so a separate Metal compiler download is not required.

```sh
cargo build --release --locked
export GEMINI_API_KEY='your-api-key'
./target/release/space /path/to/project
```

Omit the project path to use the current folder. The executable opens a GPUI window in fullscreen mode.

For development, use `cargo run --locked -- /path/to/project`. The debug profile disables optimization for application code and omits debug information. GPUI and the syntax highlighter use optimization level 2 to reduce work during rendering. The profile uses incremental compilation and 256 code generation units to reduce build time. To include full debug information, use `CARGO_PROFILE_DEV_DEBUG=2 cargo build --locked`.

To install the executable on your command path:

```sh
cargo install --path . --locked
space /path/to/project
```

The CLI selects the project folder. All chat controls are in the window.

## Chats

Space opens the last selected chat. Use the collapsible sidebar to select an earlier chat or start a new chat. Chats are stored in `.space/chats/` inside the project. Add `.space/` to the project's `.gitignore` if required. The agent cannot read or change this folder.

Use Delete beside a chat and confirm to delete its saved conversation. This action cannot be undone. Deleting the selected chat stops its run and opens the most recent readable chat. If no readable chat remains, Space creates an empty chat. Deleting another chat lets the current run continue. Unreadable chats can also be deleted.

Use Chats and Files in the thin bar at the bottom of the sidebar to change the view. In Files, click a folder to expand or collapse it. Space watches the project with native filesystem notifications and updates the tree as changes occur. Folders load when expanded. Symbolic links are listed but cannot be expanded. The private `.space` folder is hidden. Each folder shows at most 1,000 entries.

The sidebar width and input panel height are saved in `.space/ui.json` when a resize ends. Space restores these sizes when you open the same project again. The sizes adjust to keep the chat area visible in a smaller window.

Drag the handle at the top of the input panel up to increase its height, or down to decrease it. Click the expand icon in the panel's top right corner to fill the chat panel with the compose view. The sidebar stays visible if it is open. Click the restore icon or press Escape to return to the chat. The draft and the previous panel height are retained. Sending a message from the expanded view returns to the chat.

Use the mouse wheel or trackpad over the text area to scroll long drafts. Scrolling keeps its position until you type or move the cursor.

| Control | Action |
| --- | --- |
| Enter | Send a message |
| Shift+Enter | Insert a new line |
| Escape | Restore the input panel if expanded; otherwise, stop the current run |
| Stop | Stop the current run |
| Cmd+N | Start a new chat |
| Cmd+B | Show or hide the sidebar |
| Drag the right edge of the sidebar | Change the sidebar width |
| Drag the handle at the top of the input panel | Change the input panel height |
| Click the icon in the input panel's top right corner | Expand or restore the compose view |
| Cmd+W | Close the window |
| Cmd+Q | Quit |

Responses stream into the conversation with Markdown formatting. Headings, bold and italic text, strikethrough, links, lists, task lists, block quotes, code blocks, and tables are supported. Markdown links, plain HTTP and HTTPS URLs, `www.` addresses, and email addresses open when clicked. Links inside code stay as plain text. Code blocks and wide tables scroll horizontally. Images show their alternative text as a link. Each tool call appears as a compact, single-line label with its name and file path, when available. Tool calls use small, muted monospace text. Extra space separates tool groups from ARGON labels. The ARGON label uses brighter, bold text.

Successful file creation, editing, and deletion show a diff below the tool label. The diff shows line numbers, added lines in green, and removed lines in red. Click the tool label to hide or show the diff. Diffs are saved with the chat. Earlier file operations without saved diffs show only their labels. Diffs load in small groups as you scroll. Opening or closing a diff pauses automatic scrolling; use Latest to follow the response again.

Python code blocks use syntax highlighting. Mark a fenced block with `python`, `py`, `python3`, or `py3`. Highlighting updates as the response arrives. Other code blocks use plain monospace text.

Reasoning summaries appear in a collapsible Thinking section when Gemini supplies them. The section starts open. Its text streams with a fade effect and is saved with the chat. Click the arrow or Thinking to show or hide the summary. If you close the section, it stays closed as more text arrives. A stopped response retains the summary received before the stop. Opening or closing Thinking pauses automatic scrolling. Use Latest to follow the response again.

New response text fades in. The view follows the response until you scroll up. Scroll back to the bottom or use Latest to follow it again. Sending a message or opening another chat also moves the view to the bottom.

One run can be active at a time. Selecting another chat stops the current run. A file write already in progress can finish. Restarting Space restores the conversation without repeating previous file operations. A second Space process cannot open the same project at the same time.

## Project instructions

If `AGENTS.md` exists in the project root, Space includes its full UTF-8 contents in the system prompt. It reads the file before each new message, including messages in saved chats. Changes apply to the next message. Instructions remain fixed during that message's tool rounds and are saved with the chat. The file has the same 1 MiB limit and file access rules as other project files. A missing file is allowed; an unreadable or invalid file stops the run with an error.

## Agent tools

| Tool | Operation |
| --- | --- |
| `current_time()` | Return the current local date and time in RFC 3339 format, including the system timezone's UTC offset. |
| `read(path)` | Read a UTF-8 file, or list the immediate contents of a folder. Use `.` for the project root. |
| `create(path, content)` | Create a file and any missing parent folders. An existing file causes an error. |
| `edit(path, old_text, new_text)` | Replace one exact text span. The old text must be nonempty and occur exactly once. |
| `delete(path)` | Delete one regular UTF-8 file. Folders cannot be deleted. |

File changes execute directly. Paths must be relative to the project folder. The tools reject parent traversal, symbolic links, hard links, special files, and `.space` paths. File operations use directory handles. Edits replace files atomically and preserve their permission bits.

The file limit is 1 MiB. Folder listings contain at most 1,000 entries. There are no shell or move tools. The agent can change code but cannot run builds or commands.

## Google API and caching

Space reads `GEMINI_API_KEY` from the environment. It does not save the key in chats. Existing chats can be opened without a key; sending a message requires one.

Requests use the Gemini `streamGenerateContent` REST API, high thinking with reasoning summaries enabled, and a 16,384-token output limit. Each run permits up to 32 tool rounds. Failed requests and file operations are not retried automatically.

Argon supports [implicit prompt caching](https://ai.google.dev/gemini-api/docs/generate-content/caching). Space keeps system instructions and tool definitions stable during a run, then appends conversation content. It reloads `AGENTS.md` and updates tool definitions before the next message. It saves this prefix and opaque thought signatures with each chat, so reopening a chat preserves the request history.

The footer shows cumulative token counts and an estimated cost in US dollars for the selected chat. Totals include requests made during tool rounds and are saved with the chat. Cached tokens are part of the input count. Thinking tokens are charged at the output rate. A stopped or failed stream contributes any usage reported before it ended. Requests without reported usage cannot be included in the estimate.

Token counts use K for thousands and M for millions, with up to two decimal places. Counts below 1,000 stay as whole numbers. Saved counts retain full precision.

The footer also shows the latest reported context size, the 1,048,576-token window limit, and the percentage used. This count comes from the latest request's usage metadata and includes input, thinking, and output tokens. Cached input is counted once. It updates when the API reports usage and is saved with the chat. It does not include an unsent draft or tool results added after that request. A dash means no context count has been saved yet; older chats receive a count after their next reported response. See Google's [context window documentation](https://ai.google.dev/gemini-api/docs/tokens#context-window).

Estimates use Google's [standard Argon prices](https://ai.google.dev/gemini-api/docs/pricing): $2 per million input tokens, $0.10 per million cached input tokens, and $10 per million output tokens through November 30, 2026. These rates double on December 1, 2026. Space saves the cost of each request at its applicable rate, so later requests do not change earlier estimates.

Older chats stored only the last request's usage. Their totals retain those known counts and add new requests. Earlier counts cannot be recovered. Usage can also be missing after an interruption.

Google controls cache hits. Small or expired prefixes can show zero cached tokens. Space does not add filler or create explicit cache objects.

Chats use versioned JSON files with atomic saves. A chat has a 64 MiB storage limit. Damaged or unsupported chat files remain on disk and report an error when selected.

## Code layout

The library contains the model protocol, streaming client, project file tools, chat storage, and agent worker. The executable contains the GPUI window and text input. One worker thread owns all disk and network operations; the UI communicates with it through channels.
