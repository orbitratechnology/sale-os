# Catalogue import fixtures (E06-T07)

- `demo20.csv`: AT-01. 22 rows: 19 simple products, one T-shirt with 3 variants (S/M/L);
  importing it creates 20 products.
- `errors.csv`: one row per problem of PRD §16.3: a barcode twice in the file, a barcode
  that an existing product owns (the test creates "Crisps" with `5000159407236` first),
  a missing price, an unknown tax category, a short row, a negative price, a fractional
  scan quantity for an "each" item, and an unterminated quote (last row).
