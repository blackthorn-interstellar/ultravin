CREATE FUNCTION vpic.ferrorvalue(str character varying) RETURNS integer
    LANGUAGE plpgsql
    AS $$
declare
	w int = 0;
begin
	SELECT SUM(weight)
    INTO w
    FROM vpic.ErrorCode
    WHERE POSITION(',' || id::VARCHAR || ',' IN ',' || str || ',') > 0;

    RETURN COALESCE(w, 0);
end;
$$;
