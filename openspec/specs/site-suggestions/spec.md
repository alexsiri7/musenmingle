# Site Suggestions

## Purpose

Much of London's creative scene has no API and isn't known to us, but the people who go to these things know where they are listed. Letting anyone suggest a site is how coverage grows, and each suggestion turns into scraper work without manual triage.

## Requirements

### Requirement: Anyone can suggest a site and gets an immediate answer

Anyone SHALL be able to submit a website URL, with an optional short note, through the API or the site's form, and SHALL get an immediate answer: accepted, already covered (naming the source), already suggested, declined earlier (with the reason and date), rejected as invalid, or rate limited. Only http(s) URLs on a public domain SHALL be accepted; IP addresses and local host names SHALL be invalid. A suggested URL SHALL NEVER be fetched.

#### Scenario: Suggest an unknown gallery
- GIVEN no source or suggestion for example-gallery.org
- WHEN someone suggests "https://www.example-gallery.org/whats-on"
- THEN the answer is accepted

#### Scenario: Site declined earlier
- GIVEN Southbank Centre is recorded as declined because it blocks our crawler
- WHEN someone suggests a Southbank Centre URL
- THEN the answer explains when and why it was declined
- AND nothing is filed

### Requirement: A site is never queued twice

Suggestions SHALL be compared by registrable domain, so the same site written with a different path, scheme, "www" or not, SHALL be recognised as a site already covered or already suggested.

#### Scenario: Different spelling of a covered site
- GIVEN the Barbican is already a source
- WHEN someone suggests "http://barbican.org.uk/whats-on/talks"
- THEN the answer is already covered, naming the Barbican source

### Requirement: One person cannot flood the queue

Submissions from the same client SHALL be limited to 5 per hour and 20 per day, duplicates included, with a clear "try again later" giving the wait. Clients SHALL be identified only by a salted hash of their address, with IPv6 clients grouped by /64.

#### Scenario: Sixth suggestion in an hour
- GIVEN a client that made 5 submissions in the last hour
- WHEN it submits another
- THEN it is refused as rate limited with the time to wait

### Requirement: Each accepted suggestion becomes one scraper issue

Each accepted suggestion SHALL become exactly one GitHub issue labelled `new-scraper`, carrying the URL, the note and a checklist, so the autonomous worker can write that site's scraper. If filing fails, the suggestion SHALL stay pending and be filed by a later ingest run, adopting an open issue with the same title rather than duplicating it. Issues filed by the site's public forms SHALL share a daily cap; past it, submissions SHALL wait and be filed oldest first on later days, and the owner SHALL get one digest a day while any wait. A suggested site SHALL only start producing events once a hand-written scraper or platform entry for it lands.

#### Scenario: GitHub unavailable
- GIVEN GitHub is failing
- WHEN a new site is suggested
- THEN the answer is accepted without an issue number
- AND the next ingest run files exactly one new-scraper issue for it
