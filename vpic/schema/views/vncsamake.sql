CREATE VIEW vpic.vncsamake AS
 SELECT DISTINCT id,
    def AS name
   FROM vpic.defs_make
  WHERE ((id > 0) AND (EXTRACT(year FROM CURRENT_DATE) >= (from_year)::numeric) AND (EXTRACT(year FROM CURRENT_DATE) <= (COALESCE((to_year)::integer, 2999))::numeric) AND (mode = ANY (ARRAY['-1'::integer, '-257'::integer])));
