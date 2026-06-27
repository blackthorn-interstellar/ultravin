CREATE FUNCTION vpic.sqlwild_to_regex(pattern text) RETURNS text
    LANGUAGE plpgsql IMMUTABLE
    AS $_$
DECLARE
  ch   text;
  out  text := '';
BEGIN
  FOR i IN 1..length(pattern) LOOP
    ch := substr(pattern, i, 1);
    IF ch = '*' THEN
      out := out || '.';
    ELSIF ch IN ('[',']') THEN
      out := out || ch;
    ELSIF ch = '|' THEN
      out := out || '\|';
    ELSIF ch ~ '[\\\.\^\$\+\?\{\}\(\)]' THEN
      out := out || '\' || ch;
    ELSE
      out := out || ch;
    END IF;
  END LOOP;

  out = REPLACE(out, '1-A', '1A');

  RETURN '^' || out || '.*';
END;
$_$;


SET default_tablespace = '';

SET default_table_access_method = heap;
