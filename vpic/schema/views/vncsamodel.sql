CREATE VIEW vpic.vncsamodel AS
 SELECT DISTINCT ((mo.make * 1000000) + mo.id) AS id,
    mo.def AS name,
    mo.make AS makeid,
    mo.id AS originalid
   FROM (vpic.defs_make mk
     JOIN vpic.defs_model mo ON ((mk.id = mo.make)))
  WHERE ((mo.id > 0) AND (mk.id > 0) AND (EXTRACT(year FROM CURRENT_DATE) >= (mk.from_year)::numeric) AND (EXTRACT(year FROM CURRENT_DATE) <= (COALESCE((mk.to_year)::integer, 2999))::numeric) AND (EXTRACT(year FROM CURRENT_DATE) >= (mo.from_year)::numeric) AND (EXTRACT(year FROM CURRENT_DATE) <= (COALESCE((mo.to_year)::integer, 2999))::numeric) AND (mk.mode = ANY (ARRAY['-1'::integer, '-257'::integer])) AND (mo.mode = ANY (ARRAY['-1'::integer, '-257'::integer])));
