# Welcome to Karyll

This is a Markdown editor for the Kindle Scribe, and this file is an ordinary document. Edit it, rename it, or delete it. Nothing here is special.

It is also the specimen. Every kind of formatting Karyll understands appears below, so one look at this page says whether the type is set correctly on your device.

## The page is the source

Karyll never previews. The marks stay where you typed them and are drawn *quiet*, so you can see the **structure** and the prose at the same time. There is no second mode to switch into, and nothing to switch back from.

Press `Ctrl/⌘ + H` for everything the keys and the glass do.

## What it understands

Headings, from `#` to `######`:

### A third-level heading

#### A fourth

Emphasis with `*` or `_`, strong with `**`, and `code` between backticks. A [link](https://example.com) keeps its brackets. Nesting works: **strong with *emphasis* inside it**. Two tildes ~~take a phrase out~~ without deleting it.

An unordered list:

- The marker can be `-`, `*` or `+`
- Press Enter at the end of an item and the next one starts itself
- Press it again on an empty item to stop

A list of things to do, which is a list with a box after the marker:

- [ ] Enter starts the next one, always unticked
- [x] `Ctrl/⌘ + Enter` ticks the one the cursor is on, and a done one reads struck out

An ordered one:

1. Numbered items carry on the same way
2. So a list is typed rather than assembled
3. And the numbers are yours, because Karyll does not renumber behind you

> A quotation hangs from its mark rather than shifting the whole block right, so the left edge of the prose stays where the eye expects it.

A fenced block, for anything that must be read exactly as written:

```
fn main() {
    println!("no syntax colour: one bit of ink, and colour would be a lie");
}
```

Three or more dashes make a rule:

---

## 中文、日本語

简体中文是这样的。這一行是繁體字。そして日本語はまた別の字形です。

**One code point can have three correct shapes**, so each writing system is drawn in a face chosen for it. Which face you get follows the input source rather than the character, because nothing in the text itself says which convention you meant.

强调在中文里是*着重号*，日本語では*圏点*。同一句话里 *difficult* 这样的词仍然是斜体。

**Emphasis is a mark, not a slant**, because no Han face has an italic. The dot sits under each character in Chinese and over each one in Japanese; which side follows the input source, for the same reason the face does. Latin in the same sentence keeps its real *italic*.

Switch input with `Ctrl + Space`. Type pinyin or romaji, then take a candidate with the space bar or a number key.

## Getting around

Tap the left margin to go back a screen and the right margin to go on, the way a Kindle already reads. Tap the top of the page for the beginning and the foot of it for the end. None of this needs a keyboard.

`Ctrl/⌘ + Shift + O` lists every heading in the document you are in. Tap one to go there.

`Ctrl/⌘ + F` finds a word and steps through the matches. `Ctrl/⌘ + Shift + F` adds a second field, so you can change them.

The pen places the cursor between two characters, which a fingertip is too broad to do. It does not write. This is a keyboard app, and handwriting is not what it is for.

## Where your writing goes

Documents live in `/mnt/us/karyll`, outside the app. Updating Karyll replaces its own directory, and that must never take your prose with it.

There is no Save button. Karyll writes the file a few seconds after you stop typing, and again on the way out. The bar along the bottom says which document you are in, how long it is, and whether it is on disk yet. `Ctrl/⌘ + S` is there if you want to be sure.

Now delete all of this and start writing.
