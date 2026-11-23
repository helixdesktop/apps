# Calculator

Drop-in replacement for GNOME Calculator.

- **App ID**: `org.helixdesktop.Calculator`
- **Binary**: `helix-calculator`
- **Crate**: `apps/calculator`

## Purpose
Basic, advanced, financial, and programming calculator.

## Main windows
- Single window with mode switcher.
- History.

## Contracts used
- GSettings: `org.gnome.calculator`.
- No special portals required.

## Non-goals
- Exact button layout clone.
- Arbitrary-precision arithmetic. Helix uses IEEE-754 doubles for real
  arithmetic and 64-bit integers for programming mode.
- Currency conversion (needs a network rate source; tracked as later work).

---

## Behaviour (derived spec)

### Window
- One `AdwApplicationWindow`. The header bar holds a **mode switcher**
  (Basic, Advanced, Financial, Programming) and a primary menu with
  *Preferences*, *Keyboard Shortcuts* and *About*.
- Below the header: the **history** (scrollable list of past
  `expression = result` rows, newest at the bottom), then the **entry line**,
  then the button panel for the current mode.
- Clicking a history row's expression inserts it into the entry; clicking its
  result inserts the result.
- A long expression or result never widens the window: the entry scrolls
  to keep the cursor in view (the end after evaluating), a history row's
  expression and result are ellipsized at the start so their ends stay
  visible, and an error message wraps even inside a long name. The keypad
  always stays inside the window.
- Window size and the current mode persist across runs.
- Narrow windows (an `AdwBreakpoint` below 500sp; minimum 360×500): in
  Advanced, Financial and Programming mode a linked *Numbers* /
  *Functions* (/ *Bits* in Programming) toggle group above the keypad
  shows one part at a time instead of the function and number grids side
  by side; keypad buttons shrink (32 px minimum); the bit panel shows 8
  bits per row and scrolls if taller than the window. Wider than 500sp the
  side-by-side layout (and 16 bits per row) comes back. All four modes fit
  360×600 without clipping.
- Accessible names: a keypad button whose label is a symbol or short form
  is named by the operation it performs — its tooltip without the
  bracketed shortcut hint ("÷" → "Divide", "x²" → "Square", "⌫" →
  "Backspace", "C" → "Clear", "|x|" → "Absolute value", "ans" → "Last
  answer"); "(" / ")" are "Open parenthesis" / "Close parenthesis".
  Digits and hex letters keep their label as the name. Tooltips still show
  the shortcut. The mode, number base and word size drop-downs are named
  "Calculator mode", "Number base" and "Word size".
- The entry line is named "Expression". Each history row is read as one
  unit, "*expression* equals *result*", before its two buttons.
- Evaluating announces the outcome politely: "*expression* equals
  *result*". An error marks the entry invalid, is the entry's error
  message (the red caption), and is announced assertively; editing the
  entry clears all three.

### Input
- The entry accepts free typing. Everything a button does can be typed.
- `Enter` / `KP_Enter` evaluates. `Escape` clears the entry (unless a
  popover or dialog is open, which it closes instead). `Ctrl+Z` undoes the
  last evaluation while its result is still in the entry: the expression
  comes back and `ans` and variables return to their earlier values (the
  history keeps the row). Otherwise `Ctrl+Z` is the entry's own text undo.
- The entry keeps the standard editing keys: `Ctrl+A` selects all,
  `Ctrl+C`/`Ctrl+X`/`Ctrl+V` use the clipboard.
- After evaluation the entry shows the result, selected so typing a digit
  starts a new expression while typing an operator continues from it.
- Errors (syntax, division by zero, domain) are shown inline below the entry
  and leave the expression untouched with the cursor at the error position.

### Expression language
| Feature | Syntax |
|---|---|
| Operators | `+ - × ÷ * / ^ mod`, unary `-`, postfix `!` (factorial), postfix `%` (divide by 100; `a + b%` means `a + a·b/100`) |
| Superscripts | `x²`, `x³`, `x⁻¹`, any `⁰-⁹` sequence as exponent |
| Roots | `√x`, `∛x`, `n√x` (n-th root) |
| Implicit multiplication | `2π`, `3(4+1)`, `(1+2)(3+4)`, `2sin 30` |
| Constants | `π`/`pi`, `e`, `rand` (uniform [0,1)), `ans` (last result) |
| Variables | `name = expr` assigns; names are letters, not shadowing functions |
| Abs | `abs x` or `|x|` |
| Functions | `sin cos tan asin acos atan sinh cosh tanh asinh acosh atanh ln log log2 exp sqrt cbrt int frac round floor ceil sgn` (also `sin⁻¹` etc.). Arguments may be parenthesised or a single primary (`sin 30`). `log` is base 10. |
| Angles | Trig functions use the *angle unit* preference (degrees, radians, gradians). |
| Unit conversion | `<value> <unit> in <unit>` (e.g. `12 in in cm`, `100 degF in degC`) for length, area, volume, mass, time, temperature, speed, data, angle |
| Precedence (low→high) | assignment, `in`, `+ -`, `× ÷ mod` & implicit ×, unary `-`, `^` (right assoc), postfix `! %` |

### Programming mode
- Integer arithmetic on signed 64-bit values masked to the *word size*
  (8, 16, 32, 64 bits).
- Number base selector: binary, octal, decimal, hexadecimal. Input digits
  outside the current base are disabled; letters `A–F` are digits in hex.
  Literals may carry an explicit base: `0x1F`, `0b101`, `0o17`, or subscript
  `1F₁₆`.
- Additional operators: `and or xor not`, `<<`, `>>`; functions `ones`
  (one's complement), `twos` (two's complement).
- The result is shown in the selected base, with a bit panel showing the
  value's bits (clickable to toggle) grouped in nibbles: 16 per row, most
  significant first, an 8 px gap before each nibble, the bit number of each
  nibble's lowest bit under it (Adwaita `caption`, `numeric`). Each bit is
  a toggle button (at least 22×28 px) named "Bit N" whose pressed state is
  the bit's value ("Bit 5, toggle button, pressed"); the captions are
  presentational. The panel keeps its own list of toggles and their bit
  numbers (tooltips are text only).

### Financial mode
Functions available as buttons, each opening a small form, and also typeable:
| Function | Meaning |
|---|---|
| `ctrm(rate; fv; pv)` | Compounding term: periods to grow pv to fv |
| `ddb(cost; life; period)` | Double-declining depreciation for a period |
| `fv(pmt; rate; n)` | Future value of an annuity |
| `gpm(cost; margin)` | Gross-profit-margin price |
| `pmt(principal; rate; n)` | Periodic payment |
| `pv(pmt; rate; n)` | Present value of an annuity |
| `rate(fv; pv; n)` | Periodic interest rate |
| `sln(cost; salvage; life)` | Straight-line depreciation |
| `syd(cost; salvage; life; period)` | Sum-of-years-digits depreciation |
| `term(pmt; fv; rate)` | Payment periods to reach future value |

Arguments are separated with `;` (`,` is accepted too where unambiguous).

### Result formatting
Controlled by preferences:
- **Number format**: automatic (fixed, switching to scientific for very large
  or small magnitudes), fixed, scientific, engineering.
- **Accuracy**: decimal places (0–15, default 9).
- **Show thousands separators** (default off) and **show trailing zeroes**
  (default off).
- In programming mode results use the selected base.

### Decimal and group separators (locale)
- The decimal key, results and grouped digits use the locale's separators
  (from the C library's number formatting, `LC_NUMERIC`): `1.234.567,89`
  in German, `1 234 567,89` (narrow no-break spaces) in French,
  `1,234,567.89` in English. Programming mode in other bases groups binary,
  octal and hexadecimal digits in fours with spaces, as before.
- Input is read the locale's way and the English way: in a decimal-comma
  locale, typing `.` or `,` (or the keypad's decimal key) enters the
  locale's decimal separator; pasted numbers in either form are understood
  (`1.234,5` and `1,234.5` are both 1234.5; a lone `.` or `,` is the decimal
  point, except that a `.` followed by exactly three digits is a group
  separator in locales that group with `.`, so `1.500` is 1500 in German,
  as a result would be shown there). Function arguments are separated with
  `;`, or with a comma followed by a space. The engine's own form (`.`
  decimal) is internal; `--solve` reads and prints it.
- *GNOME does not document how its Calculator reads pasted numbers from
  another locale; Helix's choice:* the rules above.

### GSettings (`org.gnome.calculator`)
Keys read and written (others ignored):
`accuracy` (i), `word-size` (i), `base` (i), `show-thousands` (b),
`show-zeroes` (b), `number-format` (enum `automatic|fixed|scientific|engineering`),
`angle-units` (enum `degrees|radians|gradians`), `button-mode`
(enum `basic|advanced|financial|programming|keyboard`; `keyboard` is treated as
`basic`), `window-maximized` (b), `window-size` (`(ii)`).
If the schema is not installed, defaults are used in-memory.

### History
- Kept for the session in memory and persisted to
  `$XDG_DATA_HOME/helix-calculator/history.json` (last 100 entries).
- *Clear history* in the primary menu.

### Keyboard shortcuts
One table defines every shortcut; it registers them, fills the *Keyboard
Shortcuts* dialog (grouped Calculation, Modes, General) and is checked
against the hints in keypad tooltips, so a hint never names a key that
does nothing and no key does two things.
- Calculation: `Enter` calculate, `Escape` clear, `Ctrl+Z` undo last
  calculation, typed operators `+ - * / ^ % ! |`, `Ctrl+2` square,
  `Ctrl+R` square root, `Ctrl+P` π.
- Modes: `Alt+1`–`Alt+4` switch to Basic, Advanced, Financial and
  Programming.
- General: `Ctrl+,` preferences, `Ctrl+?` keyboard shortcuts, `Ctrl+W`
  close window, `Ctrl+Q` quit. `F1` is not bound (Calculator has no help);
  About is in the main menu.

### Command line
`helix-calculator --solve EXPR` prints the result to stdout and exits
(exit status 1 with the error on stderr on failure). `-e EXPR` opens the
window with `EXPR` in the entry.
