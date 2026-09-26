-- The project was renamed from Thaleia to Muse & Mingle: the crawler now
-- identifies as MuseNMingleBot and the repository moved to
-- alexsiri7/musenmingle (GitHub redirects the old issue URLs). Reword the
-- public refusal reasons that named the old bot, and point issue links at
-- the new repository. Data only; no schema change.
UPDATE events.refused_sources
   SET reason_text = replace(reason_text, 'the ThaleiaBot User-Agent', 'our crawler''s User-Agent')
 WHERE reason_text LIKE '%ThaleiaBot%';

UPDATE events.refused_sources
   SET issue_url = replace(issue_url, 'https://github.com/alexsiri7/thaleia/', 'https://github.com/alexsiri7/musenmingle/')
 WHERE issue_url LIKE 'https://github.com/alexsiri7/thaleia/%';
