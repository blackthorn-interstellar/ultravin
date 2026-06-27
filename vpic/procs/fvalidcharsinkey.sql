CREATE FUNCTION vpic.fvalidcharsinkey(str character varying) RETURNS TABLE(pos integer, return_chr character)
    LANGUAGE plpgsql
    AS $$
declare
	validchars varchar(50) = 'ABCDEFGHJKLMNPRSTUVWXYZ0123456789';
	strct boolean = true;
	n int = length(str);
	s char(1);
	inside boolean = false;
	ind int = 0;
	i int = 0;
	start int = 0;
	j smallint = 0;
	chars varchar(50);
	pattern varchar(50);
begin
    create temporary table IF NOT EXISTS tbl_fvalidcharsinkey (
        pos int,
        return_chr char(1)
    ) on commit drop;
	
    while i < n loop
		i = i + 1;
		s = SUBSTRING(str, i, 1);

		if s = '[' and inside = false then
		    inside = true;
			start = i;
			continue;
		end if;
	
		if inside = false then
			ind = ind + 1;

			if s = '#' then
				insert into tbl_fvalidcharsinkey values (ind, '0');
				insert into tbl_fvalidcharsinkey values (ind, '1');
				insert into tbl_fvalidcharsinkey values (ind, '2');
				insert into tbl_fvalidcharsinkey values (ind, '3');
				insert into tbl_fvalidcharsinkey values (ind, '4');
				insert into tbl_fvalidcharsinkey values (ind, '5');
				insert into tbl_fvalidcharsinkey values (ind, '6');
				insert into tbl_fvalidcharsinkey values (ind, '7');
				insert into tbl_fvalidcharsinkey values (ind, '8');
				insert into tbl_fvalidcharsinkey values (ind, '9');
				continue;
			end if;
	
			if s = '*' then
				if strct = false then
					chars = validchars;
					j = 0;
					while j < length(chars) loop
						j = j + 1;
						s = SUBSTRING (chars, j, 1);
						insert into tbl_fvalidcharsinkey values (ind, s);
					end loop;
				end if;
				continue;
			end if;
				
			insert into tbl_fvalidcharsinkey values (ind, s);
			continue;
		end if;
	
		if s = ']' and inside = true then
			ind = ind + 1;
			pattern = substring(str, start, i - start + 1);

			chars = vpic.fValidCharsInRegEx(pattern);
			j = 0;
			while j < length(chars) loop
				j = j + 1;
				s = SUBSTRING(chars, j, 1);
				if s <> '*' and s <> '|' then
					insert into tbl_fvalidcharsinkey values (ind, s);
				end if;
			end loop;
			
			inside = false;
			start = 0;
			continue;
		end if;
	end loop;
	
    return query select * from tbl_fvalidcharsinkey;
	drop table tbl_fvalidcharsinkey;
end;
$$;
