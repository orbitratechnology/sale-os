# Sinhala and Tamil: review before the pilot (D-02)

The whole till is drafted in Sinhala (`apps/desktop-ui/src/i18n/si.ts`) and Tamil
(`apps/desktop-ui/src/i18n/ta.ts`), 1,194 sentences each: the cashier screens first (302), then
on 2026-11-21 the owner and manager screens, settings, setup and the card terminal. They were machine-drafted: a native
speaker who works in Sri Lankan retail must review both before shops see them.

## How to review

- Pick the language on the sign-in screen (සිංහල / தமிழ்) and walk through a sale, a payment, a
  parked cart and a manager approval, then the owner's screens: products and import, stock,
  the drawer and Z summary, reports, sales history and returns, the audit log, every settings
  page and the setup wizard.
- Still English by design: names and data the shop types (products, staff, categories), codes
  and file formats, and messages that come from the Rust core (a few validation sentences, the
  printed receipt's own words).
- Each file maps the English sentence (left) to its translation (right). Change the right-hand
  side only. Keep every `{placeholder}` (it is replaced by a name, an amount or a number), key
  names such as F2 or Esc, and Western digits. `pnpm test` fails if a placeholder is lost.
- Shorter is better on buttons: in Tamil some labels wrap onto two lines.
- A key that starts with a context, such as `verb::Change` or `stock quantity::Change`, is the
  same English word with another meaning; the context is never shown. "Change" alone is the
  customer's change (money back).

## Terms chosen (confirm or replace; each is used in many sentences)

| English | Sinhala | Tamil |
|---|---|---|
| till | ටිල් / ටිල් එක | கவுண்டர் |
| cart | කූඩය | கூடை |
| receipt | බිල්පත | பற்றுச்சீட்டு |
| cash drawer | ලාච්චුව | காசுப் பெட்டி |
| park (a cart) | පසෙකට තබන්න | ஒதுக்கி வைக்கவும் |
| exchange | මාරු කිරීම | பொருள் மாற்றம் |
| card terminal | කාඩ් යන්ත්‍රය | அட்டை இயந்திரம் |
| printer | ප්‍රින්ටරය | அச்சுப்பொறி |
| cloud | ක්ලවුඩ් | கிளவுட் |
| manager | කළමනාකරු | முகாமையாளர் |
| change (money back) | ඉතිරි මුදල | மீதிப் பணம் |
| discount | වට්ටම | தள்ளுபடி |

## Sentences the drafts were least sure of

Owner screens (2026-11-21):

- Short words used in more than one place: "From" (a date range and a stock movement's
  source), "Full", "Partial", "Check", "Made", "Why", "Use".
- "Practice mode" / "sandbox" share one word (පුහුණු / பயிற்சி).
- Tamil: "Note" is குறிப்புரை and "Reference" குறிப்பு (they sit side by side on a stock
  document); "Write-off" is இருப்பு நீக்கம், avoiding தள்ளுபடி (discount).
- Unit names: Sinhala and Tamil add the code in brackets for "Box" and "Pack" like the others.
- The scanner-rule help keeps `price|weight|count` and `"none"` in English: they are typed into
  rules or stored.

Cashier screens:

- "Go live…" (switching from practice to real sales): Sinhala "සැබෑ විකිණීම අරඹන්න…",
  Tamil "உண்மை விற்பனைக்கு மாறவும்…".
- "No sale" (open the drawer without a sale): Sinhala "විකිණීමක් නැත", Tamil "விற்பனை இல்லை".
- "Credit {amount} towards the new items" (an exchange's returned value): check it reads as
  information, not an instruction.
- "Externally confirmed by operator" (a card or transfer the cashier recorded by hand).
- "Order discount" and "Line discounts": Tamil uses "மொத்தத் தள்ளுபடி" and "பொருள் தள்ளுபடிகள்"
  (avoiding "வரி", which also means tax).
- Fragments joined with other text: "No product with code" (followed by the code), "{items} from"
  (followed by a receipt number), "ref" (before an error reference).
- Messages that name owner screens still in English ("Settings → Scanner", “none”,
  “Show on the quick-select grid”): the drafts translated the screen names; they will match once
  those screens are translated.
