CREATE FUNCTION vpic.fvalidcharsinregex(str character varying) RETURNS character varying
    LANGUAGE plpgsql
    AS $_$
declare
	validchars varchar(50) =  'ABCDEFGHJKLMNPRSTUVWXYZ0123456789';
	result varchar(50) = '';
	i int = 0;
	n int = length(validchars);
	s char(1);
	pattern text;
begin
	str = upper(str);

	if strpos(str, '-') = 0 and strpos(str, '^') = 0 then
		return replace(replace(str, ']', ''), '[', '');
	end if;

	pattern := '^' || str || '$';
	
	while i < n loop
		i = i + 1;
		s = SUBSTRING(validchars, i, 1);
		
		if s ~ pattern then
			result = result || s;
		end if;
	end loop;
	
	return result;
end;
$_$;
