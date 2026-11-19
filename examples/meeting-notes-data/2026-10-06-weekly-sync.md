# Weekly sync 2026-10-06
Attendees: Mara Quell, Tobin Reyes, Ines Albright

## Decisions
- Keep the ingest queue on SQLite until it holds more than a million rows; Postgres adds an operator we do not have.
- Ship the lantern exporter as a separate binary.

## Blocked
- The staging certificate renewal waits on the hosting vendor's support ticket.

## Owners
- Mara Quell owns the ingest queue.
- Tobin Reyes owns the exporter.
